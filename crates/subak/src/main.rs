//! `subak` — Subak terminal CLI.
//!
//! Pre-alpha placeholder (Step 02): it only proves the workspace builds and exposes
//! `--version`/`--help`. No Host, ConPTY session or GUI exists yet (Steps 08+).
//! It starts no background process and opens no window.

use std::process::ExitCode;

const HELP: &str = "\
subak — Subak persistent terminal (pre-alpha placeholder)

USAGE:
    subak [--help | --version]

No terminal functionality is implemented yet: there is no Host, session,
attach/detach or GUI. These are planned (docs/PRODUCT_REQUIREMENTS.md, SUBAK-FR-*).
This binary starts no background process and opens no window.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        [flag] if flag == "--help" || flag == "-h" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("subak {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "subak: unsupported arguments: {}\nsubak: no commands are implemented yet; see `subak --help`",
                args.join(" ")
            );
            ExitCode::from(2)
        }
    }
}
