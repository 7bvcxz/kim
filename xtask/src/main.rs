//! Repository automation. Run `cargo xtask --help`.
//!
//! Deliberately small: std plus `serde_json` and `regex` (for contract schema checks). Never
//! shipped to users.

mod catalog_tools;
mod design;
mod docs;
mod ledger;
mod schema;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const HELP: &str = "\
cargo xtask <command>

COMMANDS:
    docs-check        Check requirement IDs, relative links and the docs index
    schema-check [--verbose]
                      Validate contract examples against docs/contracts/schemas (valid must
                      pass, examples/<name>/invalid/ must fail)
    design-check [--verbose]
                      Check design/tokens.json: colors and declared WCAG contrast pairs
    catalog-keygen --out <file>
                      Create a new catalog signing key (secret; never commit it)
    catalog-sign --key <file> [<catalog.json>]
                      Sign a catalog (default: catalog/catalog.json) -> <catalog>.sig
    ci                Run the CI gate locally: fmt --check, clippy -D warnings, tests,
                      docs-check, schema-check, design-check
    e2e [--force]     Build release binaries and run the e2e suite. Refuses unless the
                      e2e ledger says 15 Step issues were processed since the last run
                      (AGENTS.md §4); --force overrides and must be justified in the ledger.
    e2e-status        Print how many Step issues were processed since the last e2e run
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    let result = match args.first().map(String::as_str) {
        Some("docs-check") => run_docs_check(&root),
        Some("catalog-keygen") => flag_value(&args, "--out")
            .ok_or_else(|| "usage: cargo xtask catalog-keygen --out <file>".to_string())
            .and_then(|out| catalog_tools::keygen(Path::new(&out))),
        Some("catalog-sign") => flag_value(&args, "--key")
            .ok_or_else(|| {
                "usage: cargo xtask catalog-sign --key <file> [<catalog.json>]".to_string()
            })
            .and_then(|key| {
                let catalog = args
                    .iter()
                    .skip(1)
                    .rfind(|a| !a.starts_with("--") && **a != key)
                    .map_or_else(|| root.join("catalog").join("catalog.json"), PathBuf::from);
                catalog_tools::sign(Path::new(&key), &catalog)
            }),
        Some("design-check") => run_design_check(&root, args.iter().any(|a| a == "--verbose")),
        Some("schema-check") => run_schema_check(&root, args.iter().any(|a| a == "--verbose")),
        Some("ci") => run_ci(&root),
        Some("e2e") => run_e2e(&root, args.iter().any(|a| a == "--force")),
        Some("e2e-status") => e2e_status(&root).map(|n| {
            println!(
                "{n}/{} Step issues processed since the last e2e run",
                ledger::CADENCE
            );
        }),
        Some("--help" | "-h") | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("xtask: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
}

fn run_docs_check(root: &Path) -> Result<(), String> {
    let report = docs::check(root)?;
    println!(
        "docs-check: {} requirement IDs, {} markdown files, {} links checked",
        report.requirement_ids, report.files, report.links
    );
    if report.problems.is_empty() {
        println!("docs-check: PASS");
        Ok(())
    } else {
        for p in &report.problems {
            eprintln!("  - {p}");
        }
        Err(format!(
            "docs-check: FAIL ({} problems)",
            report.problems.len()
        ))
    }
}

fn run_design_check(root: &Path, verbose: bool) -> Result<(), String> {
    let report = design::check(root)?;
    if verbose {
        for (theme, fg, bg, ratio, min) in &report.pairs {
            println!("  {theme}: {fg:<22} on {bg:<20} {ratio:>6.2}:1 (min {min})");
        }
    }
    println!(
        "design-check: {} theme(s), {} colors, {} contrast pairs",
        report.themes,
        report.colors,
        report.pairs.len()
    );
    if report.problems.is_empty() {
        println!("design-check: PASS");
        Ok(())
    } else {
        for p in &report.problems {
            eprintln!("  - {p}");
        }
        Err(format!(
            "design-check: FAIL ({} problems)",
            report.problems.len()
        ))
    }
}

fn run_schema_check(root: &Path, verbose: bool) -> Result<(), String> {
    let report = schema::check_contracts(root)?;
    if verbose {
        for (path, why) in &report.rejections {
            println!("  rejected {path}: {why}");
        }
    }
    println!(
        "schema-check: {} schemas, {} valid and {} invalid examples",
        report.schemas, report.valid_examples, report.invalid_examples
    );
    if report.problems.is_empty() {
        println!("schema-check: PASS");
        Ok(())
    } else {
        for p in &report.problems {
            eprintln!("  - {p}");
        }
        Err(format!(
            "schema-check: FAIL ({} problems)",
            report.problems.len()
        ))
    }
}

fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    println!("$ cargo {}", args.join(" "));
    let status = Command::new(cargo)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed with {status}", args.join(" ")))
    }
}

fn run_ci(root: &Path) -> Result<(), String> {
    cargo(root, &["fmt", "--all", "--check"])?;
    cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    cargo(root, &["test"])?;
    run_docs_check(root)?;
    run_schema_check(root, false)?;
    run_design_check(root, false)
}

fn e2e_status(root: &Path) -> Result<u32, String> {
    let text = std::fs::read_to_string(root.join("e2e").join("LEDGER.md"))
        .map_err(|e| format!("cannot read e2e/LEDGER.md: {e}"))?;
    ledger::since_last_run(&text)
}

fn run_e2e(root: &Path, force: bool) -> Result<(), String> {
    let since = e2e_status(root)?;
    if since < ledger::CADENCE && !force {
        return Err(format!(
            "e2e suite is due after {} processed Step issues; ledger shows {since}. \
             The suite must not run routinely (AGENTS.md §4). Use --force only with a \
             recorded justification in e2e/LEDGER.md.",
            ledger::CADENCE
        ));
    }
    cargo(
        root,
        &[
            "build",
            "--release",
            "--package",
            "kim",
            "--package",
            "subak",
        ],
    )?;
    let bin_dir = root.join("target").join("release");
    let cargo_bin = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    println!("$ cargo test --package kim-e2e -- --ignored --test-threads=1");
    let status = Command::new(cargo_bin)
        .args([
            "test",
            "--package",
            "kim-e2e",
            "--",
            "--ignored",
            "--test-threads=1",
        ])
        .env("KIM_E2E", "1")
        .env("KIM_E2E_BIN_DIR", &bin_dir)
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if status.success() {
        println!("e2e: PASS — record this run in e2e/LEDGER.md");
        Ok(())
    } else {
        Err(format!(
            "e2e: FAIL ({status}) — record this run in e2e/LEDGER.md"
        ))
    }
}
