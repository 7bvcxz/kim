//! Repository automation. Run `cargo xtask --help`.
//!
//! Deliberately small: std plus `serde_json` and `regex` (for contract schema checks) and
//! `sha2` (Ddalgak consent fingerprints in the rule examples; already used by `kim`). Never
//! shipped to users.

mod catalog_tools;
mod ddalgak;
mod deps;
mod design;
mod docs;
mod hotkeys;
mod jsonschema;
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
                      pass, examples/<name>/invalid/ must fail) and recompute the hotkey
                      conflict scenarios with the reference checker; cross-check the
                      Ddalgak rule examples (DDALGAK_RULE_MODEL §8)
    deps-check        Fail if a headless package (kim, noeul, subak, subak-term…) reaches GUI crates (ADR 0014, 0023)
    design-check [--verbose]
                      Check design/tokens.json: colors and declared WCAG contrast pairs
    catalog-keygen --out <file>
                      Create a new catalog signing key (secret; never commit it)
    catalog-sign --key <file> [<catalog.json>]
                      Sign a catalog (default: catalog/catalog.json) -> <catalog>.sig
    ci                Run the CI gate locally: fmt --check, clippy -D warnings, tests,
                      docs-check, schema-check, design-check, deps-check
    e2e --scheduled   Build release binaries and run the e2e suite: the Daily GitHub
                      Actions workflow, once a day (AGENTS.md §4). Refused outside GitHub
                      Actions.
    e2e --force       Run the e2e suite locally anyway; must be justified in e2e/LEDGER.md.
    e2e-status        Print how many Step issues were processed since the last recorded run
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
        Some("deps-check") => run_deps_check(&root),
        Some("design-check") => run_design_check(&root, args.iter().any(|a| a == "--verbose")),
        Some("schema-check") => run_schema_check(&root, args.iter().any(|a| a == "--verbose")),
        Some("ci") => run_ci(&root),
        Some("e2e") => run_e2e(
            &root,
            args.iter().any(|a| a == "--force"),
            args.iter().any(|a| a == "--scheduled"),
        ),
        Some("e2e-status") => e2e_status(&root).map(|n| {
            println!(
                "{n} Step issues processed since the last recorded e2e run \
                 (the suite runs once a day in the Daily workflow)"
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

fn run_deps_check(root: &Path) -> Result<(), String> {
    let problems = deps::check(root)?;
    if problems.is_empty() {
        println!(
            "deps-check: headless packages {:?} have no GUI dependencies: PASS",
            deps::HEADLESS
        );
        Ok(())
    } else {
        for p in &problems {
            eprintln!("  - {p}");
        }
        Err(format!("deps-check: FAIL ({} problems)", problems.len()))
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
        "schema-check: {} schemas, {} valid and {} invalid examples, {} hotkey scenarios recomputed, {} Ddalgak rule documents cross-checked",
        report.schemas,
        report.valid_examples,
        report.invalid_examples,
        report.hotkey_scenarios,
        report.ddalgak_documents
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
    // Run every test binary even after a failure, so one gate run reports all failures.
    cargo(root, &["test", "--no-fail-fast"])?;
    run_docs_check(root)?;
    run_schema_check(root, false)?;
    run_design_check(root, false)?;
    run_deps_check(root)
}

fn e2e_status(root: &Path) -> Result<u32, String> {
    let text = std::fs::read_to_string(root.join("e2e").join("LEDGER.md"))
        .map_err(|e| format!("cannot read e2e/LEDGER.md: {e}"))?;
    ledger::since_last_run(&text)
}

fn run_e2e(root: &Path, force: bool, scheduled: bool) -> Result<(), String> {
    // AGENTS.md §4: the suite runs once a day in the Daily GitHub Actions workflow, never on
    // push, in hooks or in ad-hoc verification.
    let on_actions = std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
    if scheduled && !on_actions {
        return Err("`--scheduled` is for the Daily GitHub Actions workflow only".into());
    }
    if !scheduled && !force {
        return Err(
            "the e2e suite runs once a day in the Daily GitHub Actions workflow (AGENTS.md §4). \
             Use --force for a local run only with a recorded justification in e2e/LEDGER.md."
                .into(),
        );
    }
    e2e_status(root)?; // the ledger must still parse
    cargo(
        root,
        &[
            "build",
            "--release",
            "--package",
            "kim",
            "--package",
            "subak",
            "--package",
            "noeul",
            "--package",
            "ddalgak",
        ],
    )?;
    // Debug builds carry test hooks that release builds compile out: `kim` accepts a development
    // catalog key (KIM_DEV_TRUSTED_KEY); `ddalgak-gui` has the test host (no tray icon,
    // observe-only hook); `noeul` runs its overlay on an off-screen synthetic desktop
    // (NOEUL_DEV_SYNTHETIC_DESKTOP, ADR 0026); `subak-gui` has its off-screen test window
    // (`--test-offscreen`, ADR 0039) next to a debug `subak`. Some scenarios need them.
    cargo(
        root,
        &[
            "build",
            "--package",
            "kim",
            "--package",
            "ddalgak",
            "--package",
            "noeul",
            "--package",
            "subak",
        ],
    )?;
    let bin_dir = root.join("target").join("release");
    let cargo_bin = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // --no-fail-fast: one failing scenario file must not hide the results of the others.
    println!("$ cargo test --package kim-e2e --no-fail-fast -- --ignored --test-threads=1");
    let status = Command::new(cargo_bin)
        .args([
            "test",
            "--package",
            "kim-e2e",
            "--no-fail-fast",
            "--",
            "--ignored",
            "--test-threads=1",
        ])
        .env("KIM_E2E", "1")
        .env("KIM_E2E_BIN_DIR", &bin_dir)
        .env("KIM_E2E_DEBUG_BIN_DIR", root.join("target").join("debug"))
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
