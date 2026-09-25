//! Command implementations.
//!
//! Every command returns the process exit code so `main` stays a pure
//! dispatcher. Nothing here writes outside the workspace directory.
//!
//! Output goes through the macros in [`crate::ui`] rather than `println!`: a
//! closed downstream pipe (`ttk retrieve … | head`) must end the command
//! quietly, not panic, and every byte has to pass the same colour policy.

use std::io::{Read, Write};

use serde_json::json;

use ttk_core::config::{Config, Mode};
use ttk_core::event::TokenEvent;
use ttk_core::ids::SessionId;
use ttk_core::{Error, Result, tokens};
use ttk_store::Workspace;

use crate::pipeline::{self, PipelineInput, PipelineOutput};
use crate::report::{self, GainReport, StatsReport};
use crate::runner;
use crate::ui::{self, Align, errln, out, outln};

pub struct Context {
    pub mode_override: Option<Mode>,
    pub json: bool,
}

impl Context {
    pub(crate) fn load(&self) -> Result<(Config, ttk_core::config::LoadedConfig)> {
        let cwd = std::env::current_dir()?;
        let loaded = ttk_core::config::load(&cwd)?;
        let mut config = loaded.config.clone();
        if let Some(m) = self.mode_override {
            config.mode = m;
        }
        config.validate()?;
        Ok((config, loaded))
    }

    pub(crate) fn workspace(&self) -> Result<(Config, Workspace)> {
        let (config, _) = self.load()?;
        let cwd = std::env::current_dir()?;
        let ws = Workspace::open(&cwd, &config)?;
        Ok((config, ws))
    }

    pub(crate) fn print_json(&self, value: serde_json::Value) -> Result<i32> {
        outln!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(Error::other)?
        );
        Ok(0)
    }
}

/// Session id for this invocation. Reused across a shell session via
/// `TTK_SESSION`, so several `ttk run` calls aggregate into one report.
fn session_id() -> SessionId {
    match std::env::var("TTK_SESSION") {
        Ok(s) if !s.trim().is_empty() => SessionId::from_string(s.trim()),
        _ => SessionId::new(),
    }
}

/// Load the learned filter rules, or `None` when the feature is switched off.
///
/// A broken or unreadable rule file must never stop a command from running:
/// the worst outcome of a bad rule file is that nothing gets filtered, and the
/// user is told once on stderr.
fn load_rules(ws: &Workspace, config: &Config) -> Result<Option<ttk_learn::Layered>> {
    if !config.learning.enabled {
        return Ok(None);
    }
    match ttk_learn::Layered::load(ws.root()) {
        Ok(mut layered) => {
            if !config.learning.use_user_rules {
                layered.merged = layered.project.clone();
                layered.user = ttk_learn::RuleSet::default();
            }
            Ok(Some(layered))
        }
        Err(e) => {
            errln!("{}", ui::warn(format!("learned filters unavailable: {e}")));
            Ok(None)
        }
    }
}

/// Write back what the learned rules just saved.
///
/// Hits are recorded in whichever file the rule lives in, so a project rule and
/// a user rule keep separate books.
fn record_filter_hits(ws: &Workspace, config: &Config, out: &PipelineOutput) -> Result<()> {
    let Some(filtered) = &out.filtered else {
        return Ok(());
    };
    if !config.learning.record_hits || filtered.by_rule.is_empty() {
        return Ok(());
    }
    let now = ttk_core::ids::now_millis();
    let mut files = vec![ttk_learn::project_rules_path(ws.root())];
    files.extend(ttk_learn::user_rule_files());
    for path in files {
        let mut set = ttk_learn::RuleSet::load(&path)?;
        if set.is_empty() {
            continue;
        }
        let before = set.to_json();
        ttk_learn::record(&mut set, filtered, now);
        if set.to_json() != before {
            set.save(&path)?;
        }
    }
    Ok(())
}

/// Record one captured command in the global ledger.
///
/// Best effort by design: a statistic that cannot be written must never fail
/// the command the user actually ran, so a failure is one line on stderr.
fn record_global_usage(out: &PipelineOutput, program: Option<String>) {
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return;
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = ttk_core::config::find_project_root(&cwd).unwrap_or(cwd);
    let record = ttk_store::RunRecord {
        at_millis: out.event.timestamp_millis,
        project: root.display().to_string(),
        project_id: ttk_store::usage::project_id(&root),
        program,
        tokens_before: out.event.tokens_before().value,
        tokens_after: out.event.tokens_after().value,
        transformed: out.event.was_transformed(),
        filtered_lines: out.filtered.as_ref().map_or(0, |f| f.removed_lines),
        method: out.event.tokens_after().method.as_str().to_string(),
    };
    if let Err(e) = ledger.append(&record) {
        errln!("{}", ui::warn(format!("global usage not recorded: {e}")));
    }
}

fn resolve_session(ws: &Workspace, wanted: &str) -> Result<SessionId> {
    if wanted == "latest" {
        return ws.events().latest_session()?.ok_or_else(|| {
            Error::other("no sessions recorded yet — run `ttk run -- <command>` first")
        });
    }
    Ok(SessionId::from_string(wanted))
}

// ---------------------------------------------------------------------------
// init / doctor / config
// ---------------------------------------------------------------------------

pub fn help(ctx: &Context, all: bool) -> Result<i32> {
    if ctx.json {
        ctx.print_json(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "help": crate::guide::cheat_sheet(),
        }))?;
    } else if all {
        out!("{}", crate::guide::styled(&crate::guide::cheat_sheet()));
    } else {
        out!("{}", crate::guide::overview());
    }
    Ok(0)
}

/// `ttk __installer path-add|path-remove|take-over`, for the Windows installer.
///
/// Kept out of `--help`: the installer is the only caller, and it is simpler
/// for it to ask the installed binary than to carry a second copy of the
/// careful PATH editing in `path_env`.
pub fn installer_hook(action: InstallerHook) -> Result<i32> {
    use crate::path_env;
    let dir = path_env::binary_dir()?;
    match action {
        InstallerHook::PathAdd => {
            let outcome = path_env::ensure_on_path(&dir, false)?;
            outln!("{}", outcome.as_str());
        }
        InstallerHook::PathRemove => {
            let removed = path_env::remove_from_path(&dir)?;
            outln!(
                "{}",
                if removed {
                    "removed from PATH"
                } else {
                    "not on PATH"
                }
            );
        }
        // One line per action, `ok …` or `warn …`, for the installer to show.
        InstallerHook::TakeOver => {
            let report = path_env::take_over(&dir)?;
            for line in &report.done {
                outln!("ok {line}");
            }
            for line in &report.warnings {
                outln!("warn {line}");
            }
        }
        // `<action>\t<path>` per touched file; nothing when no block exists.
        InstallerHook::RefreshAgents => {
            for (path, action) in crate::install::refresh_installed(false)? {
                outln!("{}\t{}", action.as_str(), path.display());
            }
        }
    }
    Ok(0)
}

/// What `ttk __installer` can do. Mirrored in `main.rs`.
pub enum InstallerHook {
    PathAdd,
    PathRemove,
    TakeOver,
    RefreshAgents,
}

/// Write the agent instruction block into `CLAUDE.md` / `AGENTS.md`.
pub fn install(
    ctx: &Context,
    target: Option<&str>,
    scope: Option<&str>,
    yes: bool,
    dry_run: bool,
    setup_path: bool,
    compact: bool,
) -> Result<i32> {
    use crate::install::{Action, Agent, Scope, apply, choose, confirm, targets};
    use crate::path_env::{self, PathOutcome};

    let cwd = std::env::current_dir()?;
    let root = ttk_core::config::find_project_root(&cwd).unwrap_or_else(|| cwd.clone());

    // Selection: explicit flags win, otherwise ask.
    let agents = match target {
        Some(t) => Agent::parse(t)?,
        None => match choose(
            "Which coding agent should learn to use ttk?",
            &[
                "Claude Code  (CLAUDE.md)",
                "Codex        (AGENTS.md)",
                "All of them",
                "Cancel",
            ],
        )? {
            0 => vec![Agent::ClaudeCode],
            1 => vec![Agent::Codex],
            2 => Agent::ALL.to_vec(),
            _ => {
                outln!("cancelled, nothing was written");
                return Ok(0);
            }
        },
    };

    let explicit_scope = scope.is_some();
    let scope = match scope {
        Some(s) => Scope::parse(s)?,
        None if target.is_some() => Scope::Project,
        None => match choose(
            "Where should the instructions go?",
            &[
                "This project only",
                "User-global (applies to every project)",
                "Both",
                "Cancel",
            ],
        )? {
            0 => Scope::Project,
            1 => Scope::Global,
            2 => Scope::Both,
            _ => {
                outln!("cancelled, nothing was written");
                return Ok(0);
            }
        },
    };

    // Resolved before the confirmation so the plan can name the exact directory.
    let binary_dir = if setup_path {
        match path_env::binary_dir() {
            Ok(d) => Some(d),
            Err(e) => {
                errln!("ttk: PATH setup skipped: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut targets = targets(&agents, scope, &root);
    // The agent reads its global file in every project. Unless a project file
    // was asked for by name (a shared repository wants its own copy), a second
    // block there would only be read twice.
    let mut already_global = Vec::new();
    if !explicit_scope {
        targets.retain(|t| {
            let redundant = crate::install::redundant_with_global(t);
            if redundant {
                already_global.push(t.path.clone());
            }
            !redundant
        });
    }
    if targets.is_empty() && !already_global.is_empty() {
        if !ctx.json {
            outln!(
                "{}",
                ui::ok("ttk is already in your global instruction file, which every project reads")
            );
            outln!(
                "{}",
                ui::detail(
                    "nothing written, so the agent does not read it twice; \
                            pass --scope project to add a copy for a shared repository"
                )
            );
        }
        return Ok(0);
    }
    if targets.is_empty() {
        return Err(Error::other(
            "no instruction file to write (no home directory for the global scope?)",
        ));
    }

    // In `--json` mode stdout must stay a single parsable document.
    if !ctx.json {
        outln!("\nttk install will update:");
        for t in &targets {
            outln!(
                "  {:<12} {}{}",
                t.agent.label(),
                t.path.display(),
                if t.global { "   [user-global]" } else { "" }
            );
        }
        outln!(
            "\nEach file keeps its own content; only the block between the ttk markers\n\
             is added or refreshed."
        );
        if let Some(dir) = &binary_dir {
            outln!(
                "\nand put this directory on your user PATH:\n  {}\n\
                 (written through the registry / your shell profile — never with `setx`,\n\
                 which truncates a PATH longer than 1024 characters)",
                dir.display()
            );
        }
    }

    if !yes && !dry_run && !confirm("\nApply these changes?")? {
        outln!("cancelled, nothing was written");
        return Ok(0);
    }

    let mut results = Vec::new();
    for t in &targets {
        let block = crate::guide::agent_block_for(t.agent, compact);
        let action = apply(t, &block, dry_run)?;
        results.push((t.clone(), action));
        if let Some(dup) = crate::install::remove_home_duplicate(t, dry_run)? {
            results.push((
                crate::install::Target {
                    agent: t.agent,
                    path: dup,
                    global: false,
                },
                Action::DuplicateRemoved,
            ));
        }
    }

    // PATH comes last: a failure here must not undo the agent files that were
    // already written, and it is reported rather than swallowed.
    let path_outcome = match &binary_dir {
        Some(dir) => match path_env::ensure_on_path(dir, dry_run) {
            Ok(o) => Some(o),
            Err(e) => {
                errln!("ttk: {e}");
                Some(PathOutcome::Skipped {
                    reason: e.to_string(),
                })
            }
        },
        None => None,
    };

    if ctx.json {
        ctx.print_json(json!({
            "files": results
                .iter()
                .map(|(t, a)| json!({
                    "agent": t.agent.label(),
                    "path": t.path.display().to_string(),
                    "global": t.global,
                    "action": a.as_str(),
                }))
                .collect::<Vec<_>>(),
            "path_setup": path_outcome.as_ref().map(|o| json!({
                "outcome": o.as_str(),
                "detail": o.detail(),
                "directory": binary_dir.as_ref().map(|d| d.display().to_string()),
            })),
            "dry_run": dry_run,
        }))?;
        return Ok(0);
    }

    outln!("");
    for (t, action) in &results {
        outln!(
            "{:<20} {}{}",
            action.as_str(),
            t.path.display(),
            if dry_run { "   (dry run)" } else { "" }
        );
    }
    if let (Some(outcome), Some(dir)) = (&path_outcome, &binary_dir) {
        outln!(
            "{:<20} {}{}",
            outcome.as_str(),
            dir.display(),
            if outcome.detail().is_empty() {
                String::new()
            } else {
                format!("   [{}]", outcome.detail())
            }
        );
    }

    let files_unchanged = results.iter().all(|(_, a)| *a == Action::Unchanged);
    let path_changed = path_outcome.as_ref().is_some_and(|o| o.needs_new_shell());

    if dry_run {
        outln!("\ndry run: nothing was written");
    } else if files_unchanged && !path_changed {
        outln!("\nalready set up — nothing to do");
    } else {
        outln!("\ndone. Your agent now knows to run commands through `ttk run --`.");
        if path_changed {
            outln!("Open a new terminal so the updated PATH takes effect.");
        }
        outln!("Verify with:  ttk doctor      See the savings with:  ttk gain");
    }
    Ok(0)
}

/// `brief` suppresses the banner and the closing hints, for when `ttk setup`
/// is the one narrating and would otherwise say everything twice.
pub fn init(ctx: &Context, force: bool, brief: bool) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let root = ttk_core::config::find_project_root(&cwd).unwrap_or(cwd);
    let dir = root.join(ttk_store::WORKSPACE_DIR);
    std::fs::create_dir_all(&dir)?;

    if !brief {
        outln!("{}", ui::banner("initialising this project"));
    }

    let config_path = dir.join("config.toml");
    if config_path.exists() && !force {
        outln!(
            "{}",
            ui::warn(format!(
                "{} already exists (use --force to overwrite)",
                config_path.display()
            ))
        );
    } else {
        let config = Config::default();
        let mut text = String::from(
            "# ThanosTokenKiller project configuration.\n\
             # Every key below is the built-in default; delete what you do not\n\
             # want to pin. Run `ttk help` for the command overview.\n\n",
        );
        text.push_str(&config.to_toml());
        std::fs::write(&config_path, text)?;
        outln!("{}", ui::ok(format!("wrote {}", config_path.display())));
    }

    // A gitignore inside the workspace keeps captured output out of commits.
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            &ignore,
            "# Captured originals never belong in version control.\n\
             blobs/\n\
             events/\n\
             index.redb*\n\
             \n\
             # learned-filters.json is deliberately NOT ignored: the rules your\n\
             # agents taught are project knowledge and belong in the repository.\n",
        )?;
        outln!("{}", ui::ok(format!("wrote {}", ignore.display())));
    }

    let (config, _) = ctx.load()?;
    Workspace::open_at(&dir, &config)?;
    outln!(
        "{}",
        ui::ok(format!("workspace ready at {}", dir.display()))
    );
    if !brief {
        outln!();
        outln!("{}", ui::hint("run a command  ", "ttk run -- cargo test"));
        outln!(
            "{}",
            ui::hint("teach the filter", "ttk learn --last < annotated.txt")
        );
        outln!("{}", ui::hint("see the savings ", "ttk stats"));
    }
    Ok(0)
}

pub fn doctor(ctx: &Context) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let mut problems = 0;

    outln!("{}", ui::banner("installation check"));

    let loaded = match ttk_core::config::load(&cwd) {
        Ok(l) => {
            outln!(
                "{}",
                ui::kv_note(
                    "config",
                    ui::paint(ui::OK, "ok"),
                    format!("{} layer(s)", l.layers.len())
                )
            );
            Some(l)
        }
        Err(e) => {
            outln!(
                "{}",
                ui::kv("config", ui::paint(ui::BAD, format!("FAILED: {e}")))
            );
            problems += 1;
            None
        }
    };

    let config = loaded
        .as_ref()
        .map(|l| l.config.clone())
        .unwrap_or_default();
    outln!(
        "{}",
        ui::kv(
            "mode",
            ui::paint(ui::ACCENT, ctx.mode_override.unwrap_or(config.mode))
        )
    );
    outln!(
        "{}",
        ui::kv(
            "project root",
            loaded
                .as_ref()
                .and_then(|l| l.project_root.clone())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none detected)".into())
        )
    );

    let ws_path = Workspace::locate(&cwd);
    outln!("{}", ui::kv("workspace", ws_path.display()));
    match Workspace::open_at(&ws_path, &config) {
        Ok(ws) => {
            let stats = ws.capsules().stats()?;
            outln!(
                "{}",
                ui::kv_note(
                    "capsules",
                    stats.capsules,
                    format!(
                        "{} blobs, {:.1} MiB on disk, {:.1} MiB original",
                        stats.blob_count,
                        stats.blob_bytes as f64 / 1_048_576.0,
                        stats.original_bytes as f64 / 1_048_576.0
                    )
                )
            );
            outln!("{}", ui::kv("sessions", ws.events().sessions()?.len()));

            // Learned filters: how many rules are loaded, and are they on?
            match ttk_learn::Layered::load(ws.root()) {
                Ok(layered) => {
                    let enabled = layered.merged.rules.iter().filter(|r| r.enabled).count();
                    outln!(
                        "{}",
                        ui::kv_note(
                            "learned filters",
                            if config.learning.enabled {
                                ui::paint(ui::OK, format!("{enabled} active"))
                            } else {
                                ui::paint(ui::WARN, "disabled in config")
                            },
                            format!(
                                "{} project, {} user",
                                layered.project.len(),
                                layered.user.len()
                            )
                        )
                    );
                }
                Err(e) => {
                    outln!("{}", ui::kv("learned filters", ui::paint(ui::BAD, e)));
                    problems += 1;
                }
            }

            outln!(
                "{}",
                ui::kv_note(
                    "telemetry",
                    if config.telemetry.enabled {
                        "local JSONL on"
                    } else {
                        "off"
                    },
                    "never sent anywhere"
                )
            );
        }
        Err(e) => {
            outln!(
                "{}",
                ui::kv("workspace", ui::paint(ui::BAD, format!("FAILED: {e}")))
            );
            problems += 1;
        }
    }

    outln!("{}", ui::kv("compilers", ttk_compilers::registry().len()));
    outln!("{}", ui::kv("version", env!("CARGO_PKG_VERSION")));

    // Being off PATH is the single most common reason `ttk` "does not work"
    // from another directory, so doctor names it explicitly.
    // None of these count as a problem: ttk works when called by path, so this
    // must not change doctor's exit code.
    match crate::path_env::binary_dir() {
        Ok(dir) => {
            use crate::path_env::PathPresence;
            match crate::path_env::presence(&dir) {
                PathPresence::Active => outln!(
                    "{}",
                    ui::kv_note("ttk on PATH", ui::paint(ui::OK, "yes"), dir.display())
                ),
                PathPresence::PendingNewShell => {
                    outln!(
                        "{}",
                        ui::kv_note(
                            "ttk on PATH",
                            ui::paint(ui::WARN, "not in this shell"),
                            dir.display()
                        )
                    );
                    outln!(
                        "{}",
                        ui::detail(
                            "it is stored permanently; this terminal still has the environment \
                             it started with. Open a new terminal."
                        )
                    );
                }
                PathPresence::Absent => {
                    outln!(
                        "{}",
                        ui::kv_note("ttk on PATH", ui::paint(ui::WARN, "no"), dir.display())
                    );
                    outln!("{}", ui::hint("add it with", "ttk install"));
                }
                PathPresence::Unknown => {
                    outln!(
                        "{}",
                        ui::kv_note(
                            "ttk on PATH",
                            ui::paint(ui::WARN, "not in this shell"),
                            dir.display()
                        )
                    );
                    outln!("{}", ui::detail("could not read the persisted PATH."));
                }
            }
        }
        Err(e) => outln!(
            "{}",
            ui::kv("ttk on PATH", ui::paint(ui::WARN, format!("unknown: {e}")))
        ),
    }

    // The upgrade trap: an older copy earlier on the PATH means typing `ttk`
    // starts that one, whatever was just installed.
    if let Ok(me) = std::env::current_exe()
        && let Some(first) = crate::path_env::first_on_path()
        && !crate::path_env::same_file(&first, &me)
    {
        let version = std::process::Command::new(&first)
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        outln!(
            "{}",
            ui::kv_note(
                "`ttk` starts",
                ui::paint(ui::WARN, "another copy"),
                format!("{} {version}", first.display())
            )
        );
        outln!(
            "{}",
            ui::detail(
                "typing `ttk` runs that copy, not this one. Run the installer again \
                 (it replaces old copies), or remove it from your PATH."
            )
        );
    }

    outln!();
    if problems == 0 {
        outln!("{}", ui::ok("all checks passed"));
    } else {
        outln!("{}", ui::bad(format!("{problems} problem(s) found")));
    }
    Ok(if problems == 0 { 0 } else { 1 })
}

pub fn config(ctx: &Context, paths_only: bool) -> Result<i32> {
    let (config, loaded) = ctx.load()?;
    if paths_only {
        for layer in &loaded.layers {
            outln!("{layer}");
        }
        if let Some(p) = ttk_core::config::user_config_path() {
            outln!("user config would be: {}", p.display());
        }
        return Ok(0);
    }
    if ctx.json {
        ctx.print_json(json!({
            "config": serde_json::to_value(&config).map_err(Error::other)?,
            "layers": loaded.layers.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
            "project_root": loaded.project_root.as_ref().map(|p| p.display().to_string()),
        }))?;
        return Ok(0);
    }
    for layer in &loaded.layers {
        outln!("# layer: {layer}");
    }
    outln!("{}", config.to_toml());
    Ok(0)
}

// ---------------------------------------------------------------------------
// run / compile
// ---------------------------------------------------------------------------

pub fn run(ctx: &Context, argv: &[String], stream: bool) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let captured = runner::run(argv, config.limits.max_capture_bytes, stream)?;
    let content = captured.combined();

    let session = session_id();
    let rules = load_rules(&ws, &config)?;
    let mut input = PipelineInput::new(&content, session).command(&captured.context);
    if let Some(r) = &rules {
        input = input.rules(&r.merged);
    }
    let out = pipeline::process(&ws, &config, input)?;
    record_filter_hits(&ws, &config, &out)?;
    record_global_usage(&out, captured.context.program());

    if ctx.json {
        ctx.print_json(json!({
            "command": captured.context.command_line(),
            "exit_code": captured.context.exit_code,
            "duration_ms": captured.context.duration_ms,
            "content": out.content,
            "capsule": out.capsule_ref,
            "event": out.event.id.to_string(),
            "session": out.event.session_id.to_string(),
            "tokens_before": out.event.tokens_before().value,
            "tokens_after": out.event.tokens_after().value,
            "tokens_method": out.event.tokens_after().method.as_str(),
            "truncated_capture": captured.truncated(),
            "secrets_redacted": out.secrets_redacted,
            "filter": out.filtered.as_ref().map(|f| json!({
                "removed_lines": f.removed_lines,
                "blank_lines_collapsed": f.blank_lines_collapsed,
                "rules": f.by_rule.iter().map(|h| json!({
                    "id": h.id, "pattern": h.pattern, "lines": h.lines, "tokens": h.tokens, "runs": h.runs
                })).collect::<Vec<_>>(),
            })),
            "filter_protected_lines": out.protected_lines,
            "winner": out.winner,
        }))?;
    } else {
        if !stream {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(out.content.as_bytes())?;
            if !out.content.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
        }
        errln!(
            "{}",
            report::one_line(
                out.event.tokens_before(),
                out.event.tokens_after(),
                out.capsule_ref.as_deref()
            )
        );
        if let Some(line) = out.winner.as_deref().and_then(report::winner_line) {
            errln!("{line}");
        }
        if let Some(f) = &out.filtered {
            errln!("{}", report::filter_line(f));
        }
        if out.protected_lines > 0 {
            errln!(
                "{}",
                ui::warn(format!(
                    "{} line(s) matched a learned rule but were kept: they look like errors",
                    out.protected_lines
                ))
            );
        }
        if captured.truncated() {
            errln!(
                "{}",
                ui::warn(
                    "capture hit limits.max_capture_bytes; the capsule holds what was captured"
                )
            );
        }
        if out.secrets_redacted > 0 {
            errln!(
                "{}",
                ui::warn(format!(
                    "{} secret(s) replaced by placeholders; the real values stay local",
                    out.secrets_redacted
                ))
            );
        }
    }

    // Forward the child's exit code so `ttk run` is transparent in scripts.
    Ok(captured.context.exit_code.unwrap_or(1))
}

pub fn compile(ctx: &Context, file: Option<&str>, name: Option<&str>) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let rules = load_rules(&ws, &config)?;
    let (content, hint) = match file {
        Some(path) => (std::fs::read_to_string(path)?, Some(path.to_string())),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            (buf, name.map(str::to_string))
        }
    };
    if content.is_empty() {
        return Err(Error::other("nothing to compile (empty input)"));
    }

    let session = session_id();
    let mut input = PipelineInput::new(&content, session);
    if let Some(h) = &hint {
        input = input.name(h);
    }
    if let Some(r) = &rules {
        input = input.rules(&r.merged);
    }
    let out = pipeline::process(&ws, &config, input)?;
    record_filter_hits(&ws, &config, &out)?;
    record_global_usage(&out, None);

    if ctx.json {
        ctx.print_json(json!({
            "content": out.content,
            "capsule": out.capsule_ref,
            "event": out.event.id.to_string(),
            "content_type": out.detection.content_type.as_str(),
            "detection": out.detection.reason,
            "tokens_before": out.event.tokens_before().value,
            "tokens_after": out.event.tokens_after().value,
            "tokens_method": out.event.tokens_after().method.as_str(),
            "filter": out.filtered.as_ref().map(|f| json!({
                "removed_lines": f.removed_lines,
                "blank_lines_collapsed": f.blank_lines_collapsed,
                "rules": f.by_rule.iter().map(|h| json!({
                    "id": h.id, "pattern": h.pattern, "lines": h.lines, "tokens": h.tokens, "runs": h.runs
                })).collect::<Vec<_>>(),
            })),
            "filter_protected_lines": out.protected_lines,
            "winner": out.winner,
        }))?;
    } else {
        out!("{}", out.content);
        if !out.content.ends_with('\n') {
            outln!();
        }
        errln!(
            "{}",
            report::one_line(
                out.event.tokens_before(),
                out.event.tokens_after(),
                out.capsule_ref.as_deref()
            )
        );
        if let Some(line) = out.winner.as_deref().and_then(report::winner_line) {
            errln!("{line}");
        }
        if let Some(f) = &out.filtered {
            errln!("{}", report::filter_line(f));
        }
    }
    Ok(0)
}

/// `ttk read` — a file, through the same machinery as a command's output.
///
/// Two things happen that a plain `cat` cannot do:
///
/// * The file is captured, so reading the same unchanged file twice collapses
///   to a pointer instead of costing it twice.
/// * `--outline` returns the declarations and their line numbers rather than
///   the file, with the whole thing one `ttk retrieve` away.
///
/// The outline deliberately does **not** go through the quality firewall. That
/// is not an exemption carved out for convenience — it is that the firewall's
/// mandatory invariants come from `critical_regions`, a pass built to find
/// failures in *command output*. In source code the words it looks for are just
/// words: a file containing `return Err("connection refused")` is not a
/// failure, and treating it as one would make an outline impossible while
/// protecting nothing. Nothing is being replaced here either — an outline is a
/// read mode a user asked for by name, it says `[outline]` on its first line,
/// and it reports how many lines it did not show.
pub fn read(ctx: &Context, path: &str, outline: bool, lines: Option<&str>) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let content = std::fs::read_to_string(path)
        .map_err(|e| Error::other(format!("cannot read {path}: {e}")))?;

    // A synthetic command line, so an unchanged re-read is recognised as the
    // repeat it is. It is also what scopes any learned rule about this file.
    let context = ttk_compilers::CommandContext {
        argv: vec!["ttk-read".to_string(), path.to_string()],
        ..Default::default()
    };

    let rules = load_rules(&ws, &config)?;
    let mut input = PipelineInput::new(&content, session_id())
        .command(&context)
        .name(path);
    if let Some(r) = &rules {
        input = input.rules(&r.merged);
    }
    let out = pipeline::process(&ws, &config, input)?;
    record_filter_hits(&ws, &config, &out)?;
    record_global_usage(&out, Some("ttk-read".to_string()));

    // A line range is a plain slice of the file, no interpretation at all.
    if let Some(range) = lines {
        let (from, to) = parse_range(range)?;
        let sliced: String = content
            .lines()
            .enumerate()
            .filter(|(i, _)| (from..=to).contains(&(i + 1)))
            .map(|(i, l)| format!("{}: {l}\n", i + 1))
            .collect();
        out!("{sliced}");
        return Ok(0);
    }

    let view = if outline {
        let o = crate::outline::extract(&content);
        if !o.is_useful() {
            errln!(
                "{}",
                ui::warn(
                    "an outline of this file would show most of it, so the file itself is here"
                )
            );
            out.content.clone()
        } else {
            o.render(path, out.capsule_ref.as_deref())
        }
    } else {
        out.content.clone()
    };

    if ctx.json {
        return ctx.print_json(json!({
            "path": path,
            "content": view,
            "capsule": out.capsule_ref,
            "outline": outline,
            "lines": content.lines().count(),
            "tokens_before": out.event.tokens_before().value,
            "tokens_after": tokens::estimate(&view).value,
            "winner": out.winner,
        }));
    }

    out!("{view}");
    if !view.ends_with('\n') {
        outln!();
    }
    errln!(
        "{}",
        report::one_line(
            out.event.tokens_before(),
            tokens::estimate(&view),
            out.capsule_ref.as_deref()
        )
    );
    if outline {
        errln!(
            "     {}",
            ui::paint(
                ui::DIM,
                "an outline is a heuristic, not a parser — fetch any range with \
                 `ttk retrieve <cap> --lines A:B`"
            )
        );
    }
    if let Some(line) = out.winner.as_deref().and_then(report::winner_line) {
        errln!("{line}");
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// reporting
// ---------------------------------------------------------------------------

/// Lifetime totals across every recorded session in this workspace, or —
/// with `global` — across every project ttk has ever run in.
pub fn stats(ctx: &Context, session_limit: usize, global: bool) -> Result<i32> {
    if global {
        return global_stats(ctx);
    }
    let (_, ws) = ctx.workspace()?;
    let mut sessions = ws.events().sessions()?;
    if session_limit > 0 {
        sessions.truncate(session_limit);
    }

    let mut report = StatsReport::default();
    for id in &sessions {
        // One session at a time: a workspace with thousands of sessions must
        // not need all of them in memory at once.
        let events = ws.events().read_session(id)?;
        report.add_session(&events);
    }

    let storage = ws.capsules().stats()?;
    report.capsules = storage.capsules;
    report.blob_bytes = storage.blob_bytes;
    report.original_bytes = storage.original_bytes;

    if ctx.json {
        ctx.print_json(serde_json::to_value(&report).map_err(Error::other)?)?;
    } else {
        out!("{}", report.render());
    }
    Ok(0)
}

/// The number people actually want: everything ttk has saved, everywhere.
fn global_stats(ctx: &Context) -> Result<i32> {
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return Err(Error::other(
            "the global usage ledger is switched off (TTK_USAGE=0)",
        ));
    };
    let usage = ledger.aggregate()?;

    if ctx.json {
        return ctx.print_json(json!({
            "events": usage.events,
            "tokens_before": usage.tokens_before,
            "tokens_after": usage.tokens_after,
            "saved": usage.saved(),
            "saved_percent": usage.saved_percent(),
            "method": usage.method,
            "filtered_lines": usage.filtered_lines,
            "first_millis": usage.first_millis,
            "last_millis": usage.last_millis,
            "skipped_lines": usage.skipped_lines,
            "path": usage.path.as_ref().map(|p| p.display().to_string()),
            "projects": serde_json::to_value(&usage.projects).map_err(Error::other)?,
            "by_program": usage.by_program().iter().map(|(name, t)| json!({
                "program": name, "runs": t.runs, "saved": t.saved(),
            })).collect::<Vec<_>>(),
        }));
    }

    out!("{}", report::render_global(&usage));
    Ok(0)
}

/// Maintenance for the global ledger.
pub fn usage_command(ctx: &Context, action: UsageAction) -> Result<i32> {
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return Err(Error::other(
            "the global usage ledger is switched off (TTK_USAGE=0)",
        ));
    };
    match action {
        UsageAction::Path => {
            outln!("{}", ledger.path().display());
            Ok(0)
        }
        UsageAction::Compact { retain_days } => {
            let stats = ledger.compact(retain_days, ttk_core::ids::now_millis())?;
            if ctx.json {
                return ctx.print_json(json!({
                    "records_before": stats.records_before,
                    "records_after": stats.records_after,
                    "bytes_before": stats.bytes_before,
                    "bytes_after": stats.bytes_after,
                }));
            }
            outln!(
                "{}",
                ui::ok(format!(
                    "{} record(s) folded into {}, {:.1} KiB → {:.1} KiB",
                    stats.records_before,
                    stats.records_after,
                    stats.bytes_before as f64 / 1024.0,
                    stats.bytes_after as f64 / 1024.0
                ))
            );
            outln!(
                "{}",
                ui::paint(ui::DIM, "  totals are unchanged; only the detail is gone")
            );
            Ok(0)
        }
        UsageAction::Forget { project, all } => {
            if !all && project.is_none() {
                return Err(Error::other("say which project to forget, or pass --all"));
            }
            let id = project.map(|p| ttk_store::usage::project_id(std::path::Path::new(&p)));
            let dropped = ledger.forget(id.as_deref())?;
            if ctx.json {
                return ctx.print_json(json!({"dropped": dropped}));
            }
            outln!("{}", ui::ok(format!("{dropped} record(s) forgotten")));
            Ok(0)
        }
    }
}

/// What `ttk usage` can do. Mirrored in `main.rs`.
pub enum UsageAction {
    Path,
    Compact { retain_days: u64 },
    Forget { project: Option<String>, all: bool },
}

/// What `ttk gain` was asked for. Mirrored in `main.rs`.
pub struct GainArgs<'a> {
    /// A single session instead of the running total.
    pub session: Option<&'a str>,
    /// Only the project the command runs in.
    pub project: bool,
    /// Days in the chart.
    pub days: usize,
}

/// The running total: every token ttk has saved, in every project, ever.
///
/// Read from the global ledger, so it answers the same way from any directory.
/// `--session` keeps the old per-session breakdown one flag away.
pub fn gain(ctx: &Context, args: &GainArgs) -> Result<i32> {
    if let Some(session) = args.session {
        return gain_session(ctx, session);
    }
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return Err(Error::other(
            "the global usage ledger is switched off (TTK_USAGE=0)",
        ));
    };
    let mut usage = ledger.aggregate()?;

    let (project_id, scope) = if args.project {
        let cwd = std::env::current_dir()?;
        let root = ttk_core::config::find_project_root(&cwd).unwrap_or(cwd);
        let id = ttk_store::usage::project_id(&root);
        narrow_to_project(&mut usage, &id);
        (Some(id), report::short_path(&root.display().to_string()))
    } else {
        (None, "all projects".to_string())
    };

    let offset = ui::local_offset_secs();
    let today = ttk_store::usage::day_number(ttk_core::ids::now_millis(), offset);
    let daily = ledger.daily(offset, project_id.as_deref())?;
    let (filter_files, filter_rules) = global_filter_counts();

    if ctx.json {
        let window = |n: i64| {
            let (mut runs, mut before, mut after) = (0u64, 0u64, 0u64);
            for (_, d) in daily.range(today - n + 1..=today) {
                runs += d.runs;
                before += d.tokens_before;
                after += d.tokens_after;
            }
            json!({"runs": runs, "tokens_before": before, "tokens_after": after,
                   "saved": before.saturating_sub(after)})
        };
        return ctx.print_json(json!({
            "scope": scope,
            "events": usage.events,
            "tokens_before": usage.tokens_before,
            "tokens_after": usage.tokens_after,
            "saved": usage.saved(),
            "saved_percent": usage.saved_percent(),
            "method": usage.method,
            "projects": usage.projects.len(),
            "filtered_lines": usage.filtered_lines,
            "today": window(1),
            "last_7_days": window(7),
            "last_30_days": window(30),
            "daily": daily.iter().map(|(d, t)| json!({
                "date": ui::iso_date(*d), "runs": t.runs, "saved": t.saved(),
            })).collect::<Vec<_>>(),
            "by_program": usage.by_program().iter().map(|(name, t)| json!({
                "program": name, "runs": t.runs, "saved": t.saved(),
            })).collect::<Vec<_>>(),
            "global_home": ttk_core::config::global_home().map(|p| p.display().to_string()),
            "ledger": usage.path.as_ref().map(|p| p.display().to_string()),
        }));
    }

    let view = report::GainView {
        usage: &usage,
        daily: &daily,
        today,
        days: args.days,
        scope,
        all_projects: project_id.is_none(),
        home: ttk_core::config::global_home(),
        filter_files,
        filter_rules,
    };
    out!("{}", report::render_gain(&view));
    Ok(0)
}

/// Reduce a ledger aggregate to a single project, recomputing the totals.
fn narrow_to_project(usage: &mut ttk_store::GlobalUsage, project_id: &str) {
    usage.projects.retain(|p| p.project_id == project_id);
    let p = usage.projects.first();
    usage.events = p.map_or(0, |p| p.events);
    usage.tokens_before = p.map_or(0, |p| p.tokens_before);
    usage.tokens_after = p.map_or(0, |p| p.tokens_after);
    usage.filtered_lines = p.map_or(0, |p| p.filtered_lines);
    usage.first_millis = p.map(|p| p.first_millis);
    usage.last_millis = p.map(|p| p.last_millis);
}

/// Global filter files, and the rules they hold between them.
fn global_filter_counts() -> (usize, usize) {
    let files: Vec<_> = ttk_learn::user_rule_files()
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    let rules = files
        .iter()
        .filter_map(|p| ttk_learn::RuleSet::load(p).ok())
        .map(|s| s.len())
        .sum();
    (files.len(), rules)
}

/// The text dropped into a fresh global folder, so it explains itself to
/// whoever opens it in Explorer.
const GLOBAL_README: &str = "\
ThanosTokenKiller — global folder
=================================

Everything here applies to every project on this machine.

  filters\\            global filter rules. Every *.json rule file in this
                      folder is loaded by every `ttk run`, in every project.
                      `ttk learn --user` writes to filters\\learned-filters.json;
                      `ttk rules export` produces files you can drop in here.
  usage.jsonl         the savings ledger: one line per `ttk run`, anywhere.
                      `ttk gain` adds it up. It never leaves this machine.
  config.toml         optional user configuration (see `ttk config --paths`).

Useful commands

  ttk gain            how many tokens ttk has saved, in total
  ttk global --open   open this folder
  ttk rules           the learned rules, ranked by what they saved
";

/// Show, create or open the global folder.
pub fn global(ctx: &Context, init: bool, open: bool) -> Result<i32> {
    let Some(home) = ttk_core::config::global_home() else {
        return Err(Error::other(
            "this platform has no configuration directory, so there is no global folder",
        ));
    };
    let filters = home.join(ttk_core::config::GLOBAL_FILTERS_DIR);
    if init || open {
        std::fs::create_dir_all(&filters)?;
        let readme = home.join("README.txt");
        if !readme.exists() {
            std::fs::write(
                &readme,
                GLOBAL_README.replace('\n', if cfg!(windows) { "\r\n" } else { "\n" }),
            )?;
        }
        // Resolving the rule path also moves a legacy rule file into place.
        let _ = ttk_learn::user_rules_path();
    }

    let ledger = ttk_store::UsageLedger::open();
    let usage = ledger.as_ref().and_then(|l| l.aggregate().ok());
    let (filter_files, filter_rules) = global_filter_counts();

    if ctx.json {
        return ctx.print_json(json!({
            "home": home.display().to_string(),
            "exists": home.is_dir(),
            "filters_dir": filters.display().to_string(),
            "filter_files": filter_files,
            "filter_rules": filter_rules,
            "ledger": ledger.as_ref().map(|l| l.path().display().to_string()),
            "saved": usage.as_ref().map(|u| u.saved()),
        }));
    }

    outln!("{}", ui::banner("global folder"));
    outln!();
    outln!("{}", ui::kv("folder", ui::paint(ui::CODE, home.display())));
    if !home.is_dir() {
        outln!(
            "{}",
            ui::detail("does not exist yet — `ttk global --init` creates it")
        );
    }
    outln!(
        "{}",
        ui::kv_note(
            "filters",
            format!(
                "{} rule{} in {} file{}",
                filter_rules,
                if filter_rules == 1 { "" } else { "s" },
                filter_files,
                if filter_files == 1 { "" } else { "s" }
            ),
            filters.display()
        )
    );
    for file in ttk_learn::user_rule_files().iter().filter(|p| p.is_file()) {
        let n = ttk_learn::RuleSet::load(file).map(|s| s.len()).unwrap_or(0);
        outln!(
            "  {}{}  {}",
            " ".repeat(ui::KEY_WIDTH),
            ui::paint(ui::DIM, ui::glyphs().corner),
            format!(
                "{}  {}",
                file.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default(),
                ui::paint(
                    ui::DIM,
                    format!("{n} rule{}", if n == 1 { "" } else { "s" })
                )
            )
        );
    }
    match (&ledger, &usage) {
        (Some(l), Some(u)) => outln!(
            "{}",
            ui::kv_note(
                "savings ledger",
                format!(
                    "{} tokens saved in {} runs",
                    ui::paint(
                        ui::SAVED,
                        ui::tokens(u.saved(), u.method != "provider" && u.method != "tokenizer")
                    ),
                    ui::thousands(u.events)
                ),
                l.path().display()
            )
        ),
        _ => outln!("{}", ui::kv_note("savings ledger", "off", "TTK_USAGE=0")),
    }
    outln!();
    outln!("{}", ui::hint("open it      ", "ttk global --open"));
    outln!("{}", ui::hint("add a filter ", "ttk learn --last --user"));
    outln!("{}", ui::hint("see the total", "ttk gain"));

    if open {
        open_in_file_manager(&home);
    }
    Ok(0)
}

/// Best effort: a folder that cannot be opened was still printed above.
fn open_in_file_manager(dir: &std::path::Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .is_err()
    {
        errln!("{}", ui::warn(format!("could not start {program}")));
    }
}

fn gain_session(ctx: &Context, session: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let id = resolve_session(&ws, session)?;
    let events = ws.events().read_session(&id)?;
    let report = GainReport::build(id.as_str(), &events);
    if ctx.json {
        ctx.print_json(serde_json::to_value(&report).map_err(Error::other)?)?;
    } else {
        out!("{}", report.render());
    }
    Ok(0)
}

fn find_event(ws: &Workspace, id: &str) -> Result<TokenEvent> {
    ws.events()
        .find_event(id)?
        .ok_or_else(|| Error::other(format!("no event matching `{id}`")))
}

pub fn inspect(ctx: &Context, id: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let event = find_event(&ws, id)?;
    if ctx.json {
        ctx.print_json(serde_json::to_value(&event).map_err(Error::other)?)?;
        return Ok(0);
    }
    outln!("{}", ui::banner("event"));
    outln!("{}", ui::kv("event", ui::paint(ui::ACCENT, &event.id)));
    outln!("{}", ui::kv("session", &event.session_id));
    outln!("{}", ui::kv("source", event.source.as_str()));
    outln!("{}", ui::kv("content type", event.content_type.as_str()));
    outln!("{}", ui::kv("trust", event.trust_level.as_str()));
    outln!("{}", ui::kv("sensitivity", event.sensitivity.as_str()));
    outln!(
        "{}",
        ui::kv("hash", ttk_core::short_hash(&event.source_hash))
    );
    outln!(
        "{}",
        ui::kv_note(
            "tokens",
            format!(
                "{} {} {}",
                event.tokens_before(),
                ui::glyphs().arrow,
                ui::paint(ui::NUM, event.tokens_after())
            ),
            format!("saved {}", event.tokens_saved())
        )
    );
    if let Some(c) = &event.capsule_id {
        outln!(
            "{}",
            ui::kv("capsule", ui::paint(ui::DIM, format!("cap://{c}")))
        );
    }
    if !event.invariants.is_empty() {
        outln!(
            "{}",
            ui::kv(
                "invariants",
                format!("{} preserved", event.invariants.len())
            )
        );
    }
    if !event.metadata.is_empty() {
        outln!("{}", ui::heading("metadata"));
        for (k, v) in &event.metadata {
            outln!("{}", ui::kv(k, v));
        }
    }
    Ok(0)
}

pub fn explain(ctx: &Context, id: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let event = find_event(&ws, id)?;

    if ctx.json {
        ctx.print_json(json!({
            "event": event.id.to_string(),
            "transformations": serde_json::to_value(&event.transformations).map_err(Error::other)?,
            "invariants": serde_json::to_value(&event.invariants).map_err(Error::other)?,
        }))?;
        return Ok(0);
    }

    outln!(
        "{}",
        ui::banner(&format!("why {} looks like that", event.id))
    );
    outln!(
        "{}",
        ui::kv_note(
            "detected as",
            event.content_type.as_str(),
            event
                .metadata
                .get("detection")
                .and_then(|v| v.as_str())
                .unwrap_or("-")
        )
    );
    if event.transformations.is_empty() {
        outln!();
        outln!(
            "{}",
            ui::paint(
                ui::DIM,
                "no transformation was attempted — the content was passed through unchanged."
            )
        );
        return Ok(0);
    }
    for (i, t) in event.transformations.iter().enumerate() {
        let accepted = matches!(
            t.validation,
            ttk_core::event::ValidationOutcome::Passed
                | ttk_core::event::ValidationOutcome::PassedWithWarnings
        );
        outln!(
            "{}",
            ui::heading(&format!("step {}: {} v{}", i + 1, t.transformer, t.version))
        );
        outln!(
            "{}",
            ui::kv(
                "tokens",
                format!(
                    "{} {} {}",
                    t.tokens_before,
                    ui::glyphs().arrow,
                    ui::paint(ui::NUM, t.tokens_after)
                )
            )
        );
        outln!(
            "{}",
            ui::kv(
                "validation",
                if accepted {
                    ui::paint(ui::OK, format!("{:?}", t.validation))
                } else {
                    ui::paint(ui::WARN, format!("{:?}", t.validation))
                }
            )
        );
        outln!(
            "{}",
            ui::kv_note(
                "hashes",
                ttk_core::short_hash(&t.input_hash),
                format!(
                    "{} {}",
                    ui::glyphs().arrow,
                    ttk_core::short_hash(&t.output_hash)
                )
            )
        );
        if let Some(f) = &t.fallback {
            outln!("{}", ui::warn(f.summary()));
            outln!("{}", ui::detail("the original content was kept"));
        }
        if !t.invariants.is_empty() {
            outln!(
                "{}",
                ui::kv("guaranteed", format!("{} invariant(s)", t.invariants.len()))
            );
            for inv in t.invariants.iter().take(12) {
                outln!(
                    "      {} {}",
                    ui::paint(ui::DIM, format!("[{}]", inv.kind.as_str())),
                    ui::ellipsize(&inv.value, 68)
                );
            }
            if t.invariants.len() > 12 {
                outln!(
                    "{}",
                    ui::detail(format!("{} more", t.invariants.len() - 12))
                );
            }
        }
        for n in &t.notes {
            outln!("{}", ui::kv("note", n));
        }
    }
    if let Some(c) = &event.capsule_id {
        outln!();
        outln!(
            "{}",
            ui::hint("full original", &format!("ttk retrieve {c} --level 4"))
        );
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// capsules
// ---------------------------------------------------------------------------

pub fn retrieve(
    ctx: &Context,
    capsule: &str,
    level: u8,
    lines: Option<&str>,
    allow_secrets: bool,
) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let text = match lines {
        Some(range) => {
            let (from, to) = parse_range(range)?;
            ws.capsules().lines(capsule, from, to, allow_secrets)?
        }
        None => ws.capsules().render(capsule, level, allow_secrets)?,
    };
    out!("{text}");
    if !text.ends_with('\n') {
        outln!();
    }
    Ok(0)
}

fn parse_range(range: &str) -> Result<(usize, usize)> {
    let (a, b) = range
        .split_once(':')
        .ok_or_else(|| Error::other("line range must look like `100:180`"))?;
    let from: usize = a
        .trim()
        .parse()
        .map_err(|_| Error::other(format!("invalid start line `{a}`")))?;
    let to: usize = b
        .trim()
        .parse()
        .map_err(|_| Error::other(format!("invalid end line `{b}`")))?;
    if to < from {
        return Err(Error::other("line range end is before its start"));
    }
    Ok((from, to))
}

pub fn search(
    ctx: &Context,
    capsule: &str,
    needle: &str,
    max: usize,
    allow_secrets: bool,
) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let hits = ws.capsules().search(capsule, needle, max, allow_secrets)?;
    if ctx.json {
        ctx.print_json(json!(
            hits.iter()
                .map(|(n, l)| json!({"line": n, "text": l}))
                .collect::<Vec<_>>()
        ))?;
    } else {
        for (n, line) in &hits {
            outln!("{}{line}", ui::paint(ui::DIM, format!("{n}: ")));
        }
        if hits.is_empty() {
            errln!("{}", ui::bad(format!("no match for `{needle}`")));
            return Ok(1);
        }
    }
    Ok(0)
}

pub fn raw(ctx: &Context, capsule: &str, allow_secrets: bool) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let bytes = ws.capsules().raw(capsule, allow_secrets)?;
    std::io::stdout().lock().write_all(&bytes)?;
    Ok(0)
}

pub fn capsule_list(ctx: &Context, limit: usize) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let capsules = ws.capsules().list(limit)?;
    if ctx.json {
        ctx.print_json(serde_json::to_value(&capsules).map_err(Error::other)?)?;
        return Ok(0);
    }
    if capsules.is_empty() {
        outln!("{}", ui::paint(ui::DIM, "no capsules yet"));
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = capsules
        .iter()
        .map(|c| {
            vec![
                ui::paint(ui::ACCENT, &c.short),
                c.id.to_string(),
                c.content_type.as_str().to_string(),
                ui::thousands(c.original_bytes),
                if c.requires_raw_gate() {
                    ui::paint(ui::WARN, format!("{} [gated]", c.sensitivity.as_str()))
                } else {
                    ui::paint(ui::DIM, c.sensitivity.as_str())
                },
            ]
        })
        .collect();
    out!(
        "{}",
        ui::table(
            &["handle", "capsule", "content", "bytes", "sensitivity"],
            &[
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Left
            ],
            &rows
        )
    );
    Ok(0)
}

pub fn capsule_stats(ctx: &Context) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let s = ws.capsules().stats()?;
    if ctx.json {
        ctx.print_json(json!({
            "capsules": s.capsules,
            "blobs": s.blob_count,
            "blob_bytes": s.blob_bytes,
            "original_bytes": s.original_bytes,
        }))?;
        return Ok(0);
    }
    outln!("{}", ui::banner("capsule store"));
    outln!("{}", ui::kv_num("capsules", s.capsules, ""));
    outln!("{}", ui::kv_num("blobs", s.blob_count, ""));
    outln!(
        "{}",
        ui::kv_num(
            "on disk",
            format!("{:.2} MiB", s.blob_bytes as f64 / 1_048_576.0),
            ""
        )
    );
    outln!(
        "{}",
        ui::kv_num(
            "original size",
            format!("{:.2} MiB", s.original_bytes as f64 / 1_048_576.0),
            ""
        )
    );
    if s.blob_bytes > 0 {
        outln!(
            "{}",
            ui::kv_num(
                "storage ratio",
                ui::paint(
                    ui::OK,
                    format!("{:.1}x", s.original_bytes as f64 / s.blob_bytes as f64)
                ),
                "deduplicated + compressed"
            )
        );
    }
    Ok(0)
}

pub fn capsule_gc(ctx: &Context) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let stats = ws.capsules().gc(ttk_core::ids::now_millis())?;
    if ctx.json {
        ctx.print_json(json!({
            "expired_removed": stats.expired_removed,
            "evicted_for_size": stats.evicted_for_size,
            "bytes_freed": stats.bytes_freed,
        }))?;
    } else {
        outln!(
            "{}",
            ui::ok(format!(
                "removed {} expired, evicted {} for size, freed {:.2} MiB",
                stats.expired_removed,
                stats.evicted_for_size,
                stats.bytes_freed as f64 / 1_048_576.0
            ))
        );
    }
    Ok(0)
}

pub fn capsule_delete(ctx: &Context, capsule: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    if ws.capsules().delete(capsule)? {
        outln!("{}", ui::ok(format!("deleted {capsule}")));
        Ok(0)
    } else {
        errln!("{}", ui::bad(format!("no capsule matching `{capsule}`")));
        Ok(1)
    }
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

/// Re-run the current compilers over the stored originals of a session.
///
/// This is how a change to a compiler is validated against real data: same
/// input, new code, reported difference. Nothing is overwritten.
pub fn replay(ctx: &Context, session: &str) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let id = resolve_session(&ws, session)?;
    let events = ws.events().read_session(&id)?;
    if events.is_empty() {
        return Err(Error::other(format!("session `{id}` has no events")));
    }

    let mut rows = Vec::new();
    for event in &events {
        let Some(capsule_id) = &event.capsule_id else {
            continue;
        };
        let original = match ws
            .capsules()
            .render(capsule_id.as_str(), ttk_store::LEVEL_RAW, true)
        {
            Ok(t) => t,
            // An expired capsule is a legitimate outcome, not a failure.
            Err(_) => continue,
        };
        // The same short handle the live run used, or replay would measure a
        // longer reference against a shorter recorded one and call it a
        // regression.
        let capsule_ref = ws
            .capsules()
            .get(capsule_id.as_str())
            .map(|c| c.reference())
            .unwrap_or_else(|_| format!("cap://{capsule_id}"));
        let input = ttk_compilers::CompileInput::new(&original, &config)
            .content_type(event.content_type)
            .source(event.source)
            .capsule(&capsule_ref);
        let now = match ttk_compilers::compile(&input) {
            Some(candidate) => {
                let verdict =
                    ttk_core::firewall::Firewall::new(&config).review(&original, candidate);
                tokens::estimate(&verdict.output)
            }
            None => tokens::estimate(&original),
        };
        rows.push((
            event.id.to_string(),
            event.tokens_before().value,
            event.tokens_after().value,
            now.value,
        ));
    }

    if ctx.json {
        ctx.print_json(json!(
            rows.iter()
                .map(|(id, before, then, now)| json!({
                    "event": id, "tokens_before": before, "tokens_then": then, "tokens_now": now
                }))
                .collect::<Vec<_>>()
        ))?;
        return Ok(0);
    }

    outln!("{}", ui::banner(&format!("replay of session {id}")));
    let (mut then_total, mut now_total) = (0u64, 0u64);
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|(id, before, then, now)| {
            then_total += then;
            now_total += now;
            let delta = match now.cmp(then) {
                std::cmp::Ordering::Less => ui::paint(ui::OK, format!("-{}", then - now)),
                std::cmp::Ordering::Greater => ui::paint(ui::WARN, format!("+{}", now - then)),
                std::cmp::Ordering::Equal => ui::paint(ui::DIM, "="),
            };
            vec![
                id.clone(),
                ui::thousands(*before),
                ui::thousands(*then),
                ui::thousands(*now),
                delta,
            ]
        })
        .collect();
    out!(
        "{}",
        ui::table(
            &["event", "before", "recorded", "now", "delta"],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right
            ],
            &table_rows
        )
    );
    outln!();
    outln!("{}", ui::kv("replayed", format!("{} event(s)", rows.len())));
    match now_total.cmp(&then_total) {
        std::cmp::Ordering::Less => outln!(
            "{}",
            ui::ok(format!(
                "the current compilers are {} tokens better than the recorded run",
                ui::thousands(then_total - now_total)
            ))
        ),
        std::cmp::Ordering::Greater => outln!(
            "{}",
            ui::warn(format!(
                "the current compilers are {} tokens worse than the recorded run",
                ui::thousands(now_total - then_total)
            ))
        ),
        std::cmp::Ordering::Equal => {
            outln!("{}", ui::ok("identical to the recorded run"))
        }
    }
    Ok(0)
}
