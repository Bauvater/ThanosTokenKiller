//! `ttk` — the ThanosTokenKiller command line interface.
//!
//! Exit codes are stable and scriptable:
//! * `ttk run` forwards the child's exit code,
//! * everything else returns 0 on success and the code from
//!   [`ttk_core::Error::exit_code`] on failure.

mod commands;
mod guide;
mod install;
mod path_env;
mod pipeline;
mod report;
mod runner;

use clap::{Parser, Subcommand};
use ttk_core::config::Mode;

#[derive(Parser)]
#[command(
    name = "ttk",
    version,
    about = "ThanosTokenKiller — the smallest provably sufficient context.",
    long_about = "ThanosTokenKiller captures verbose tool output, compiles it into a compact \
                  Token IR, verifies that nothing critical was lost, and keeps the byte exact \
                  original in a local capsule you can always retrieve.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Override the operating mode for this invocation.
    #[arg(long, global = true, value_name = "MODE")]
    mode: Option<String>,

    /// Machine readable output where the command supports it.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the command overview and the current savings table.
    Help,
    /// Create `.ttk/` and a project configuration file.
    Init {
        /// Overwrite an existing configuration.
        #[arg(long)]
        force: bool,
    },
    /// Teach a coding agent (Claude Code, Codex) to route commands through ttk.
    ///
    /// Without `--target` this asks interactively. The agent's instruction file
    /// is never overwritten: a single marked block is added or updated.
    Install {
        /// Which agent: `claude`, `codex` or `all`. Omit to choose interactively.
        #[arg(long, value_name = "AGENT")]
        target: Option<String>,
        /// Where: `project` (default), `global` or `both`.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Do not add the ttk binary's directory to the user PATH.
        #[arg(long)]
        no_path: bool,
    },
    /// Check the installation, the workspace and the effective configuration.
    Doctor,
    /// Show the effective configuration and where each layer came from.
    Config {
        /// Print the config file paths only.
        #[arg(long)]
        paths: bool,
    },
    /// Run a command and compile its output.
    Run {
        /// Also stream the raw output to the terminal while it runs.
        #[arg(long)]
        stream: bool,
        /// The command to run, after `--`.
        #[arg(trailing_var_arg = true, required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },
    /// Compile content from stdin or a file.
    Compile {
        /// Read from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// Name hint for content detection (used with stdin).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// Lifetime totals for this workspace: how much has been saved, over how
    /// many commands.
    Stats {
        /// Only look at the newest N sessions (0 = all).
        #[arg(long, default_value_t = 0, value_name = "N")]
        sessions: usize,
    },
    /// Report token savings for a session.
    Gain {
        /// Session id, or `latest`.
        #[arg(long, default_value = "latest", value_name = "SESSION")]
        session: String,
    },
    /// Show a single token event.
    Inspect {
        /// Event id or unique prefix.
        event: String,
    },
    /// Explain what was done to an event and why.
    Explain {
        /// Event id or unique prefix.
        event: String,
    },
    /// Retrieve a capsule at a given detail level.
    Retrieve {
        capsule: String,
        /// 0 = one line … 4 = byte exact original.
        #[arg(long, default_value_t = 2)]
        level: u8,
        /// Line range of the original, e.g. `100:180`.
        #[arg(long, value_name = "FROM:TO")]
        lines: Option<String>,
        /// Allow reading a capsule that contains detected secrets.
        #[arg(long)]
        allow_secrets: bool,
    },
    /// Search inside a capsule's original content.
    Search {
        capsule: String,
        needle: String,
        #[arg(long, default_value_t = 50)]
        max: usize,
        #[arg(long)]
        allow_secrets: bool,
    },
    /// Print the byte exact original of a capsule.
    Raw {
        capsule: String,
        #[arg(long)]
        allow_secrets: bool,
    },
    /// Inspect and maintain the capsule store.
    Capsule {
        #[command(subcommand)]
        action: CapsuleAction,
    },
    /// Re-run the compilers over a stored session and compare the results.
    Replay {
        #[arg(long, default_value = "latest", value_name = "SESSION")]
        session: String,
    },
}

#[derive(Subcommand)]
enum CapsuleAction {
    /// List capsules, newest first.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Storage statistics.
    Stats,
    /// Drop expired capsules and enforce the size budget.
    Gc,
    /// Delete one capsule.
    Delete { capsule: String },
}

fn main() {
    let cli = Cli::parse();
    let mode_override = match cli.mode.as_deref().map(str::parse::<Mode>) {
        Some(Ok(m)) => Some(m),
        Some(Err(e)) => {
            eprintln!("ttk: {e}");
            std::process::exit(e.exit_code());
        }
        None => None,
    };

    let ctx = commands::Context {
        mode_override,
        json: cli.json,
    };

    let result = match cli.command {
        Command::Help => commands::help(&ctx),
        Command::Init { force } => commands::init(&ctx, force),
        Command::Install {
            target,
            scope,
            yes,
            dry_run,
            no_path,
        } => commands::install(
            &ctx,
            target.as_deref(),
            scope.as_deref(),
            yes,
            dry_run,
            !no_path,
        ),
        Command::Doctor => commands::doctor(&ctx),
        Command::Config { paths } => commands::config(&ctx, paths),
        Command::Run { stream, argv } => commands::run(&ctx, &argv, stream),
        Command::Compile { file, name } => {
            commands::compile(&ctx, file.as_deref(), name.as_deref())
        }
        Command::Stats { sessions } => commands::stats(&ctx, sessions),
        Command::Gain { session } => commands::gain(&ctx, &session),
        Command::Inspect { event } => commands::inspect(&ctx, &event),
        Command::Explain { event } => commands::explain(&ctx, &event),
        Command::Retrieve {
            capsule,
            level,
            lines,
            allow_secrets,
        } => commands::retrieve(&ctx, &capsule, level, lines.as_deref(), allow_secrets),
        Command::Search {
            capsule,
            needle,
            max,
            allow_secrets,
        } => commands::search(&ctx, &capsule, &needle, max, allow_secrets),
        Command::Raw {
            capsule,
            allow_secrets,
        } => commands::raw(&ctx, &capsule, allow_secrets),
        Command::Capsule { action } => match action {
            CapsuleAction::List { limit } => commands::capsule_list(&ctx, limit),
            CapsuleAction::Stats => commands::capsule_stats(&ctx),
            CapsuleAction::Gc => commands::capsule_gc(&ctx),
            CapsuleAction::Delete { capsule } => commands::capsule_delete(&ctx, &capsule),
        },
        Command::Replay { session } => commands::replay(&ctx, &session),
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("ttk: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
