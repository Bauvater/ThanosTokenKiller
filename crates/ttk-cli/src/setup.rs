//! `ttk setup` — the first five minutes.
//!
//! `ttk install` does one job well: it writes the agent instruction block and
//! puts the binary on `PATH`. That is the right thing for somebody who already
//! knows what ttk is. It is the wrong first screen for somebody who just
//! downloaded it, because it answers a question they have not asked yet.
//!
//! This walks through the whole thing in order, explains each step in one line
//! before doing it, and ends by actually running a command so the very first
//! thing a new user sees is their own output getting smaller.
//!
//! Every step is skippable and every step is idempotent: running `ttk setup`
//! twice is a no-op with a report, not a mess.

use std::path::Path;

use ttk_core::Result;

use crate::commands::Context;
use crate::install::{self, Action, Agent, Scope};
use crate::ui::{self, errln, outln};

/// One numbered step of the walkthrough.
fn step(n: usize, of: usize, title: &str) -> String {
    format!(
        "\n{} {}",
        ui::paint(ui::ACCENT, format!("[{n}/{of}]")),
        ui::paint(ui::HEAD, title)
    )
}

pub struct SetupArgs {
    /// Accept every default without asking. What the install scripts use.
    pub yes: bool,
    /// Do not touch `PATH`.
    pub no_path: bool,
    /// Skip the closing demonstration run.
    pub no_demo: bool,
}

pub fn run(ctx: &Context, args: &SetupArgs) -> Result<i32> {
    const STEPS: usize = 5;
    let cwd = std::env::current_dir()?;
    let root = ttk_core::config::find_project_root(&cwd).unwrap_or_else(|| cwd.clone());

    outln!("{}", ui::banner("setup"));
    outln!();
    outln!(
        "  {}",
        ui::paint(
            ui::HEAD,
            "Your agent reads thousands of tokens of tool output to learn one thing."
        )
    );
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "ttk compiles that output down, keeps the byte exact original nearby, and\n\
             \x20 learns from your agent which lines were never worth sending."
        )
    );
    outln!();
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "Nothing here talks to the network. Everything it writes is listed below."
        )
    );

    // -- 1. the workspace ---------------------------------------------------
    outln!("{}", step(1, STEPS, "a workspace for this project"));
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "captured output, the event log and the rules your agents teach"
        )
    );
    let workspace_dir = root.join(ttk_store::WORKSPACE_DIR);
    outln!("{}", ui::kv("directory", workspace_dir.display()));
    if args.yes || install::confirm("  Create it?")? {
        crate::commands::init(ctx, false, true)?;
    } else {
        outln!("{}", ui::paint(ui::DIM, "  skipped"));
    }

    // -- 2. the agent -------------------------------------------------------
    outln!("{}", step(2, STEPS, "teach your agent to use it"));
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "one marked block in CLAUDE.md / AGENTS.md; your own rules are untouched"
        )
    );
    let agents = if args.yes {
        Agent::ALL.to_vec()
    } else {
        match install::choose(
            "  Which agent?",
            &[
                "Claude Code  (CLAUDE.md)",
                "Codex        (AGENTS.md)",
                "Both",
                "Skip",
            ],
        )? {
            0 => vec![Agent::ClaudeCode],
            1 => vec![Agent::Codex],
            2 => Agent::ALL.to_vec(),
            _ => Vec::new(),
        }
    };
    let compact = if agents.is_empty() || args.yes {
        false
    } else {
        install::choose(
            "  How much should it be told?",
            &[
                "Everything     (~800 tokens, teaches the whole loop)",
                "The essentials (~200 tokens, in every request you ever make)",
            ],
        )? == 1
    };
    let mut written = Vec::new();
    if !agents.is_empty() {
        for target in install::targets(&agents, Scope::Project, &root) {
            let block = crate::guide::agent_block_for(target.agent, compact);
            let action = install::apply(&target, &block, false)?;
            written.push((target.path.clone(), action));
        }
        for (path, action) in &written {
            outln!(
                "  {} {}",
                ui::paint(
                    if *action == Action::Unchanged {
                        ui::DIM
                    } else {
                        ui::OK
                    },
                    format!("{:<10}", action.as_str())
                ),
                path.display()
            );
        }
    } else {
        outln!("{}", ui::paint(ui::DIM, "  skipped"));
    }

    // -- 3. PATH ------------------------------------------------------------
    outln!("{}", step(3, STEPS, "make `ttk` runnable from anywhere"));
    if args.no_path || std::env::var("TTK_NO_PATH_SETUP").is_ok() {
        outln!("{}", ui::paint(ui::DIM, "  skipped by request"));
    } else {
        match crate::path_env::binary_dir() {
            Ok(dir) => {
                outln!("{}", ui::kv("directory", dir.display()));
                outln!(
                    "  {}",
                    ui::paint(
                        ui::DIM,
                        if cfg!(windows) {
                            "added to the user PATH through the registry — never `setx`, \
                             which truncates a long PATH"
                        } else {
                            "one marked block appended to your shell profile"
                        }
                    )
                );
                if args.yes || install::confirm("  Add it to your PATH?")? {
                    match crate::path_env::ensure_on_path(&dir, false) {
                        Ok(outcome) => {
                            outln!("  {}", ui::paint(ui::OK, outcome.as_str()));
                            if outcome.needs_new_shell() {
                                outln!(
                                    "{}",
                                    ui::detail("open a new terminal for this to take effect")
                                );
                            }
                        }
                        Err(e) => errln!("{}", ui::warn(e)),
                    }
                } else {
                    outln!("{}", ui::paint(ui::DIM, "  skipped"));
                }
            }
            Err(e) => outln!("{}", ui::warn(format!("cannot locate the binary: {e}"))),
        }
    }

    // -- 4. where the numbers live -----------------------------------------
    outln!(
        "{}",
        step(4, STEPS, "one running total, across every project")
    );
    match ttk_store::usage::global_dir() {
        Some(dir) => {
            outln!(
                "{}",
                ui::kv("ledger", dir.join(ttk_store::usage::USAGE_FILE).display())
            );
            outln!(
                "  {}",
                ui::paint(
                    ui::DIM,
                    "every `ttk run`, in any repository, lands here — so `ttk gain`\n\
                     \x20 can answer \"how much has this saved me\" without hunting through\n\
                     \x20 twenty projects. It holds local paths and never leaves the machine;\n\
                     \x20 `TTK_USAGE=0` switches it off, `ttk usage forget --all` empties it."
                )
            );
        }
        None => outln!(
            "{}",
            ui::warn("this platform has no configuration directory, so there is no global ledger")
        ),
    }

    // -- 5. proof ----------------------------------------------------------
    outln!("{}", step(5, STEPS, "see it work"));
    if args.no_demo || (!args.yes && !install::confirm("  Run a command through it now?")?) {
        outln!("{}", ui::paint(ui::DIM, "  skipped"));
    } else {
        demonstrate(ctx, &root)?;
    }

    // -- the card ----------------------------------------------------------
    outln!("{}", ui::heading("that is all of it"));
    outln!(
        "{}",
        ui::hint("run anything      ", "ttk run -- cargo test")
    );
    outln!(
        "{}",
        ui::hint("read a file       ", "ttk read --outline src/main.rs")
    );
    outln!("{}", ui::hint("find noise to kill", "ttk suggest"));
    outln!("{}", ui::hint("teach the filter  ", "ttk learn --last"));
    outln!("{}", ui::hint("your running total", "ttk gain"));
    outln!("{}", ui::hint("everything else   ", "ttk help"));
    outln!();
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "check the installation any time with `ttk doctor`."
        )
    );
    Ok(0)
}

/// Run something harmless and show the saving on real output.
///
/// A number in a README is a claim; a number about the machine it is printed on
/// is evidence. The command is chosen to exist everywhere and to produce enough
/// output to be worth compiling.
fn demonstrate(ctx: &Context, root: &Path) -> Result<i32> {
    let argv = demo_command(root);
    if argv.is_empty() {
        outln!(
            "{}",
            ui::paint(
                ui::DIM,
                "  nothing obvious to run here — try `ttk run -- <command>`"
            )
        );
        return Ok(0);
    }
    outln!(
        "  {} {}",
        ui::paint(ui::DIM, "$"),
        ui::paint(ui::CODE, format!("ttk run -- {}", argv.join(" ")))
    );
    outln!();
    crate::commands::run(ctx, &argv, false)
}

/// Pick a command that exists in this project and prints something.
fn demo_command(root: &Path) -> Vec<String> {
    let exists = |p: &str| root.join(p).exists();
    let cmd = |parts: &[&str]| parts.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

    if root.join(".git").exists() {
        return cmd(&["git", "status"]);
    }
    if exists("Cargo.toml") {
        return cmd(&["cargo", "--version"]);
    }
    if exists("package.json") {
        return cmd(&["npm", "--version"]);
    }
    if exists("pyproject.toml") || exists("requirements.txt") {
        return cmd(&["python", "--version"]);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_picks_something_that_exists() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(demo_command(dir.path()).is_empty(), "nothing to run yet");

        std::fs::write(dir.path().join("Cargo.toml"), "").expect("write");
        assert_eq!(demo_command(dir.path()), vec!["cargo", "--version"]);

        std::fs::create_dir(dir.path().join(".git")).expect("mkdir");
        assert_eq!(
            demo_command(dir.path()),
            vec!["git", "status"],
            "a repository is the most interesting output on offer"
        );
    }

    #[test]
    fn steps_are_numbered_for_the_reader_not_the_machine() {
        let text = step(2, 5, "teach your agent");
        assert!(text.contains("[2/5]"), "{text}");
        assert!(text.contains("teach your agent"));
    }
}
