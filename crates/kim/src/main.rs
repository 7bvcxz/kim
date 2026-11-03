//! `kim` — the Kim manager.
//!
//! Pre-alpha placeholder (Step 02): it only proves the workspace builds and exposes
//! `--version`/`--help`. Catalog, install and update commands arrive in Steps 05–07.
//! It never touches the network, installs anything, or opens a GUI.

use std::process::ExitCode;

const HELP: &str = "\
kim — Kim manager (pre-alpha placeholder)

USAGE:
    kim [--help | --version]

No management commands are implemented yet. Catalog, install, update and
uninstall commands are planned (docs/PRODUCT_REQUIREMENTS.md, KIM-FR-003..010).
This binary never installs, updates or starts other tools.
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
            println!("kim {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "kim: unsupported arguments: {}\nkim: no commands are implemented yet; see `kim --help`",
                args.join(" ")
            );
            ExitCode::from(2)
        }
    }
}
