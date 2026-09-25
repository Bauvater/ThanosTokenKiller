//! `ttk-setup.exe` — the Windows installer for ThanosTokenKiller.
//!
//! One file to download and double-click. It carries `ttk.exe` inside itself
//! and installs it for the current user only, so it never asks for
//! administrator rights:
//!
//! ```text
//! %LOCALAPPDATA%\Programs\ttk\ttk.exe          the program
//! %LOCALAPPDATA%\Programs\ttk\uninstall.exe    a copy of this installer
//! %APPDATA%\ttk\                               the global folder
//!     filters\                                 global filters, every project
//!     usage.jsonl                              every token saved, everywhere
//! ```
//!
//! plus an entry on the user `PATH`, Start menu shortcuts and an entry in
//! *Settings → Apps* so it can be removed like any other program.
//!
//! The careful parts — editing `PATH` without truncating it, creating the
//! global folder — are delegated to the freshly installed `ttk.exe`, so there
//! is exactly one implementation of each.
//!
//! Flags: `--yes` (no questions), `--dir <path>`, `--no-path`, `--no-agent`,
//! `--uninstall`. A copy named `uninstall.exe` uninstalls by default.

#[cfg(windows)]
mod setup;

#[cfg(windows)]
fn main() {
    std::process::exit(setup::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ttk-setup is the Windows installer. On Linux and macOS use install.sh.");
    std::process::exit(1);
}
