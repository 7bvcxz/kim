//! `subak-gui` — the Subak window (Step 16, ADR 0039, docs/SUBAK_GUI.md).
//!
//! A pure client of the per-user Subak Host (ADR 0009): it shows one Terminal's pane in a native
//! Win32 window drawn by the `subak-term` CPU renderer. Closing it, or the process ending in any
//! way, is a detach; the Host and its Terminals keep running. The Host and the `subak` CLI never
//! need this program.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    subak::gui::main()
}
