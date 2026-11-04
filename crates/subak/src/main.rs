//! `subak` — Subak terminal CLI and Host. Everything lives in the package's library
//! (`src/lib.rs`), which `subak-gui` shares.

fn main() -> std::process::ExitCode {
    subak::cli_main()
}
