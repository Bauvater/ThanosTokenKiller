//! `ttk` — the ThanosTokenKiller command line interface.
//!
//! Exit codes are stable and scriptable:
//! * `ttk run` forwards the child's exit code,
//! * everything else returns 0 on success and the code from
//!   [`ttk_core::Error::exit_code`] on failure.

mod ui;

mod commands;
mod delta;
mod guide;
mod install;
mod learncmd;
mod outline;
mod path_env;
mod pipeline;
mod report;
mod runner;
mod setup;
mod suggest;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use ttk_core::config::Mode;

/// `--help` in the same colours as the rest of ttk.
const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightMagenta.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::BrightMagenta.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::BrightCyan.on_default())
    .placeholder(AnsiColor::Cyan.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

#[derive(Parser)]
#[command(
    name = "ttk",
    version,
    about = "ThanosTokenKiller — the smallest provably sufficient context.",
    long_about = "ThanosTokenKiller captures verbose tool output, compiles it into a compact \
                  Token IR, verifies that nothing critical was lost, and keeps the byte exact \
                  original in a local capsule you can always retrieve.",
    disable_help_subcommand = true,
    styles = HELP_STYLES
)]
struct Cli {
    /// Override the operating mode for this invocation.
    #[arg(long, global = true, value_name = "MODE")]
    mode: Option<String>,

    /// Machine readable output where the command supports it.
    #[arg(long, global = true)]
    json: bool,

    /// Colour: `auto` (default), `always` or `never`.
    ///
    /// `auto` styles a terminal and stays plain in a pipe; `NO_COLOR` and
    /// `CLICOLOR_FORCE` are honoured without this flag.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    color: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the command overview; `--all` for every command and flag.
    Help {
        /// The complete reference instead of the overview.
        #[arg(long, short = 'a')]
        all: bool,
    },
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
        /// Write the short instruction block (~200 tokens instead of ~800).
        ///
        /// That block is in every request the agent makes, so on a long session
        /// the difference is larger than it looks.
        #[arg(long)]
        compact: bool,
    },
    /// Walk through everything at once: workspace, agent, PATH, first run.
    ///
    /// What to run after downloading ttk. `ttk install` does one step of this
    /// and is what you want once you already know the tool.
    Setup {
        /// Accept every default without asking.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Do not add the ttk binary's directory to the user PATH.
        #[arg(long)]
        no_path: bool,
        /// Skip the closing demonstration run.
        #[arg(long)]
        no_demo: bool,
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
    /// Teach the filter which lines are worthless, using `<filter-trash>` tags.
    ///
    /// Paste the output back with the junk wrapped in `<filter-trash>` …
    /// `</filter-trash>`. Everything you leave unmarked becomes a
    /// counter-example, so no rule can be learned that would remove it.
    Learn {
        /// Read the annotated text from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// Learn from the newest captured command, taking its scope from it.
        #[arg(long)]
        last: bool,
        /// Learn from the output stored in this capsule.
        #[arg(long, value_name = "CAPSULE", conflicts_with = "last")]
        capsule: Option<String>,
        /// Learn from the output of this event.
        #[arg(long, value_name = "EVENT", conflicts_with_all = ["last", "capsule"])]
        event: Option<String>,
        /// Scope the rules explicitly, e.g. `--scope "npm install"`.
        #[arg(long, value_name = "COMMAND")]
        scope: Option<String>,
        /// Apply the rules to every command, not just this one.
        #[arg(long, conflicts_with = "scope")]
        global: bool,
        /// Widen the scope from `npm install` to all of `npm`.
        #[arg(long, conflicts_with_all = ["scope", "global"])]
        program: bool,
        /// Store in the user-global rule file instead of the project's.
        #[arg(long)]
        user: bool,
        /// Show what would be learned without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Learn even from lines that look like errors. Rarely right.
        #[arg(long)]
        force: bool,
        /// A note stored with the new rules.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Inspect and maintain the learned filter rules.
    Rules {
        #[command(subcommand)]
        action: Option<RulesAction>,
    },
    /// Look for repeating noise in what ttk already stored, and draft a lesson.
    ///
    /// Nothing is learned: the output is a `<filter-trash>` document to read,
    /// edit and pipe into `ttk learn`.
    Suggest {
        /// Only look at this command, e.g. `--scope "npm install"`.
        #[arg(long, value_name = "COMMAND")]
        scope: Option<String>,
        /// A shape has to appear in at least this many runs to be suggested.
        #[arg(long, default_value_t = 2, value_name = "N")]
        min_runs: u64,
        /// 0 = no limit.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// Print only the draft lesson, ready to pipe into `ttk learn`.
        #[arg(long)]
        lesson: bool,
    },
    /// Run the learned filter over text without learning or storing anything.
    Filter {
        /// Read from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// Pretend the text came from this command, e.g. `--scope "npm install"`.
        #[arg(long, value_name = "COMMAND")]
        scope: Option<String>,
        /// Report which rules fired instead of printing the filtered text.
        #[arg(long)]
        explain: bool,
    },
    /// Read a file, captured and de-duplicated like any other output.
    ///
    /// Reading the same unchanged file twice costs a pointer the second time.
    /// `--outline` returns the declarations and their line numbers instead of
    /// the file; the whole file stays one `ttk retrieve` away.
    Read {
        /// The file to read.
        path: String,
        /// Declarations and line numbers instead of the file itself.
        #[arg(long, short = 'o')]
        outline: bool,
        /// A numbered slice of the file, e.g. `--lines 100:180`.
        #[arg(long, value_name = "FROM:TO", conflicts_with = "outline")]
        lines: Option<String>,
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
    /// Lifetime totals: how much has been saved, over how many commands.
    Stats {
        /// Only look at the newest N sessions (0 = all).
        #[arg(long, default_value_t = 0, value_name = "N")]
        sessions: usize,
        /// Every project ttk has ever run in, not just this one.
        #[arg(long, short = 'g')]
        global: bool,
    },
    /// Maintain the global usage ledger that `ttk stats --global` reads.
    Usage {
        #[command(subcommand)]
        action: UsageCmd,
    },
    /// How many tokens ttk has saved: in total, per day, per command.
    ///
    /// Adds up the global ledger, so it covers every project on this machine
    /// and answers the same from any directory.
    Gain {
        /// Only the project in the current directory.
        #[arg(long, short = 'p')]
        project: bool,
        /// Days in the chart.
        #[arg(long, default_value_t = 14, value_name = "N")]
        days: usize,
        /// One session of this workspace in detail instead (`latest` or an id).
        #[arg(long, value_name = "SESSION")]
        session: Option<String>,
    },
    /// The global folder: filters for every project, and the savings ledger.
    Global {
        /// Create the folder and its `filters` directory.
        #[arg(long)]
        init: bool,
        /// Open it in the file manager (creates it first).
        #[arg(long)]
        open: bool,
    },
    /// Used by the Windows installer; not meant to be typed.
    #[command(name = "__installer", hide = true)]
    Installer {
        #[command(subcommand)]
        action: InstallerAction,
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
enum InstallerAction {
    /// Put this binary's directory on the user PATH.
    PathAdd,
    /// Take this binary's directory off the user PATH.
    PathRemove,
    /// Replace every older ttk on the PATH with this one.
    TakeOver,
    /// Update every installed agent block to this version; remove duplicates.
    RefreshAgents,
}

#[derive(Subcommand)]
enum UsageCmd {
    /// Print the path of the ledger.
    Path,
    /// Fold old records into per-project rollups. Totals are unchanged.
    Compact {
        #[arg(long, default_value_t = 45, value_name = "DAYS")]
        retain_days: u64,
    },
    /// Remove a project's history from the ledger, or all of it.
    Forget {
        /// Project directory to forget.
        #[arg(value_name = "PATH")]
        project: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum RulesAction {
    /// Show the rules, ranked by what they actually saved.
    List {
        /// Include disabled rules.
        #[arg(long)]
        all: bool,
        /// 0 = no limit.
        #[arg(long, default_value_t = 30, value_name = "N")]
        limit: usize,
    },
    /// Everything about one rule.
    Show { rule: String },
    /// Stop a rule from firing, keeping it on record.
    Disable { rule: String },
    /// Let a disabled rule fire again.
    Enable { rule: String },
    /// Delete a rule from the rule file.
    Forget { rule: String },
    /// Drop rules that have never fired.
    Prune {
        #[arg(long, default_value_t = 90, value_name = "DAYS")]
        unused_days: u64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Print every rule as JSON, for `ttk rules import` elsewhere.
    Export,
    /// Merge a rule file into this project's.
    Import {
        /// Read from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// Import into the user-global rule file.
        #[arg(long)]
        user: bool,
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

    if let Err(e) = ui::set_color_choice(&cli.color) {
        eprintln!("ttk: {e}");
        std::process::exit(e.exit_code());
    }

    let ctx = commands::Context {
        mode_override,
        json: cli.json,
    };

    let Some(command) = cli.command else {
        // A bare `ttk` is somebody finding out what this is.
        let code = match commands::help(&ctx, false) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("ttk: {e}");
                e.exit_code()
            }
        };
        std::process::exit(code);
    };

    let result = match command {
        Command::Help { all } => commands::help(&ctx, all),
        Command::Init { force } => commands::init(&ctx, force, false),
        Command::Install {
            target,
            scope,
            yes,
            dry_run,
            no_path,
            compact,
        } => commands::install(
            &ctx,
            target.as_deref(),
            scope.as_deref(),
            yes,
            dry_run,
            !no_path,
            compact,
        ),
        Command::Setup {
            yes,
            no_path,
            no_demo,
        } => setup::run(
            &ctx,
            &setup::SetupArgs {
                yes,
                no_path,
                no_demo,
            },
        ),
        Command::Doctor => commands::doctor(&ctx),
        Command::Config { paths } => commands::config(&ctx, paths),
        Command::Run { stream, argv } => commands::run(&ctx, &argv, stream),
        Command::Learn {
            file,
            last,
            capsule,
            event,
            scope,
            global,
            program,
            user,
            dry_run,
            force,
            note,
        } => learncmd::learn(
            &ctx,
            &learncmd::LearnArgs {
                file: file.as_deref(),
                capsule: capsule.as_deref(),
                event: event.as_deref(),
                last,
                scope: scope.as_deref(),
                global,
                program_only: program,
                user,
                dry_run,
                force,
                note: note.as_deref(),
            },
        ),
        Command::Rules { action } => match action.unwrap_or(RulesAction::List {
            all: false,
            limit: 30,
        }) {
            RulesAction::List { all, limit } => learncmd::rules_list(&ctx, all, limit),
            RulesAction::Show { rule } => learncmd::rules_show(&ctx, &rule),
            RulesAction::Disable { rule } => learncmd::rules_set_enabled(&ctx, &rule, false),
            RulesAction::Enable { rule } => learncmd::rules_set_enabled(&ctx, &rule, true),
            RulesAction::Forget { rule } => learncmd::rules_forget(&ctx, &rule),
            RulesAction::Prune {
                unused_days,
                dry_run,
            } => learncmd::rules_prune(&ctx, unused_days, dry_run),
            RulesAction::Export => learncmd::rules_export(&ctx),
            RulesAction::Import { file, user } => {
                learncmd::rules_import(&ctx, file.as_deref(), user)
            }
        },
        Command::Suggest {
            scope,
            min_runs,
            limit,
            lesson,
        } => learncmd::suggest(&ctx, scope.as_deref(), min_runs, limit, lesson),
        Command::Filter {
            file,
            scope,
            explain,
        } => learncmd::filter(&ctx, file.as_deref(), scope.as_deref(), explain),
        Command::Read {
            path,
            outline,
            lines,
        } => commands::read(&ctx, &path, outline, lines.as_deref()),
        Command::Compile { file, name } => {
            commands::compile(&ctx, file.as_deref(), name.as_deref())
        }
        Command::Stats { sessions, global } => commands::stats(&ctx, sessions, global),
        Command::Usage { action } => commands::usage_command(
            &ctx,
            match action {
                UsageCmd::Path => commands::UsageAction::Path,
                UsageCmd::Compact { retain_days } => commands::UsageAction::Compact { retain_days },
                UsageCmd::Forget { project, all } => commands::UsageAction::Forget { project, all },
            },
        ),
        Command::Gain {
            project,
            days,
            session,
        } => commands::gain(
            &ctx,
            &commands::GainArgs {
                session: session.as_deref(),
                project,
                days,
            },
        ),
        Command::Global { init, open } => commands::global(&ctx, init, open),
        Command::Installer { action } => commands::installer_hook(match action {
            InstallerAction::PathAdd => commands::InstallerHook::PathAdd,
            InstallerAction::PathRemove => commands::InstallerHook::PathRemove,
            InstallerAction::TakeOver => commands::InstallerHook::TakeOver,
            InstallerAction::RefreshAgents => commands::InstallerHook::RefreshAgents,
        }),
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
