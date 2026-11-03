//! `subak` — Subak terminal CLI.
//!
//! Pre-alpha placeholder (Step 02): it proves the workspace builds and exposes `--version`,
//! `--help` and `capabilities` (docs/CLI_CONTRACT.md §9), which honestly reports that no terminal
//! command exists yet. No Host, ConPTY session or GUI exists (Steps 08+). It starts no
//! background process and opens no window. The real CLI (Step 12) follows the same contract.

use std::io::Write;
use std::process::ExitCode;

const HELP: &str = "\
subak — Subak persistent terminal (pre-alpha placeholder)

USAGE:
    subak [--help | --version]
    subak capabilities [--json]

No terminal functionality is implemented yet: there is no Host, session,
attach/detach or GUI. These are planned (docs/PRODUCT_REQUIREMENTS.md, SUBAK-FR-*).
`subak capabilities --json` reports this to other tools and agents.
This binary starts no background process and opens no window.
";

/// The capabilities document (`kim.capabilities` v1). Its `version` must equal the package
/// version; a test checks it and keeps docs/contracts/examples/tool-capabilities-v1/subak.json
/// identical.
const CAPABILITIES: &str = include_str!("capabilities.json");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let mut out = std::io::stdout().lock();
    match words.as_slice() {
        [] if !json => {
            let _ = write!(out, "{HELP}");
            ExitCode::SUCCESS
        }
        ["--help" | "-h"] => {
            let _ = write!(out, "{HELP}");
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            let _ = writeln!(out, "subak {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["capabilities"] if json => {
            let _ = writeln!(
                out,
                "{{\"ok\":true,\"command\":\"capabilities\",\"schema_version\":1,\"data\":{},\"warnings\":[]}}",
                CAPABILITIES.trim()
            );
            ExitCode::SUCCESS
        }
        ["capabilities"] => {
            let _ = write!(
                out,
                "subak {} — pre-alpha placeholder (CLI contract v1)\n\
                 No terminal commands are implemented yet; the only command is `capabilities`.\n",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        _ => {
            if json {
                let _ = writeln!(
                    out,
                    "{{\"ok\":false,\"command\":\"subak\",\"schema_version\":1,\"error\":{{\"code\":\"usage\",\"exit_code\":2,\"message\":\"unsupported arguments; no terminal commands are implemented yet\",\"hint\":\"see `subak --help`\",\"details\":[]}}}}"
                );
            } else {
                eprintln!(
                    "subak: error: unsupported arguments: {}\nhint: no terminal commands are implemented yet; see `subak --help`",
                    args.join(" ")
                );
            }
            ExitCode::from(2)
        }
    }
}
