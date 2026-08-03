//! Command implementations.
//!
//! Every command returns the process exit code so `main` stays a pure
//! dispatcher. Nothing here writes outside the workspace directory.
//!
//! Output goes through [`outln!`] / [`out!`] rather than `println!`: a closed
//! downstream pipe (`ttk retrieve … | head`) must end the command quietly, not
//! panic.

/// `println!` that tolerates a closed stdout.
macro_rules! outln {
    () => {{ let _ = { use std::io::Write; writeln!(std::io::stdout()) }; }};
    ($($t:tt)*) => {{ let _ = { use std::io::Write; writeln!(std::io::stdout(), $($t)*) }; }};
}

/// `print!` that tolerates a closed stdout.
macro_rules! out {
    ($($t:tt)*) => {{ let _ = { use std::io::Write; write!(std::io::stdout(), $($t)*) }; }};
}

use std::io::{Read, Write};

use serde_json::json;

use ttk_core::config::{Config, Mode};
use ttk_core::event::TokenEvent;
use ttk_core::ids::SessionId;
use ttk_core::{Error, Result, tokens};
use ttk_store::Workspace;

use crate::pipeline::{self, PipelineInput};
use crate::report::{self, GainReport, StatsReport};
use crate::runner;

pub struct Context {
    pub mode_override: Option<Mode>,
    pub json: bool,
}

impl Context {
    fn load(&self) -> Result<(Config, ttk_core::config::LoadedConfig)> {
        let cwd = std::env::current_dir()?;
        let loaded = ttk_core::config::load(&cwd)?;
        let mut config = loaded.config.clone();
        if let Some(m) = self.mode_override {
            config.mode = m;
        }
        config.validate()?;
        Ok((config, loaded))
    }

    fn workspace(&self) -> Result<(Config, Workspace)> {
        let (config, _) = self.load()?;
        let cwd = std::env::current_dir()?;
        let ws = Workspace::open(&cwd, &config)?;
        Ok((config, ws))
    }

    fn print_json(&self, value: serde_json::Value) -> Result<()> {
        outln!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(Error::other)?
        );
        Ok(())
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

pub fn help(ctx: &Context) -> Result<i32> {
    if ctx.json {
        ctx.print_json(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "help": crate::guide::cheat_sheet(),
        }))?;
    } else {
        out!("{}", crate::guide::cheat_sheet());
    }
    Ok(0)
}

/// Write the agent instruction block into `CLAUDE.md` / `AGENTS.md`.
pub fn install(
    ctx: &Context,
    target: Option<&str>,
    scope: Option<&str>,
    yes: bool,
    dry_run: bool,
    setup_path: bool,
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
                eprintln!("ttk: PATH setup skipped: {e}");
                None
            }
        }
    } else {
        None
    };

    let targets = targets(&agents, scope, &root);
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
        let block = crate::guide::agent_block(t.agent);
        let action = apply(t, &block, dry_run)?;
        results.push((t.clone(), action));
    }

    // PATH comes last: a failure here must not undo the agent files that were
    // already written, and it is reported rather than swallowed.
    let path_outcome = match &binary_dir {
        Some(dir) => match path_env::ensure_on_path(dir, dry_run) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("ttk: {e}");
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
        outln!("Verify with:  ttk doctor      See the savings with:  ttk stats");
    }
    Ok(0)
}

pub fn init(ctx: &Context, force: bool) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let root = ttk_core::config::find_project_root(&cwd).unwrap_or(cwd);
    let dir = root.join(ttk_store::WORKSPACE_DIR);
    std::fs::create_dir_all(&dir)?;

    let config_path = dir.join("config.toml");
    if config_path.exists() && !force {
        outln!(
            "{} already exists (use --force to overwrite)",
            config_path.display()
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
        outln!("wrote {}", config_path.display());
    }

    // A gitignore inside the workspace keeps captured output out of commits.
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            &ignore,
            "# Captured originals never belong in version control.\nblobs/\nevents/\nindex.redb*\n",
        )?;
        outln!("wrote {}", ignore.display());
    }

    let (config, _) = ctx.load()?;
    Workspace::open_at(&dir, &config)?;
    outln!("workspace ready at {}", dir.display());
    outln!("try:  ttk run -- cargo test");
    Ok(0)
}

pub fn doctor(ctx: &Context) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let mut problems = 0;

    let loaded = match ttk_core::config::load(&cwd) {
        Ok(l) => {
            outln!("config           ok ({} layer(s))", l.layers.len());
            Some(l)
        }
        Err(e) => {
            outln!("config           FAILED: {e}");
            problems += 1;
            None
        }
    };

    let config = loaded
        .as_ref()
        .map(|l| l.config.clone())
        .unwrap_or_default();
    outln!(
        "mode             {}",
        ctx.mode_override.unwrap_or(config.mode)
    );
    outln!(
        "project root     {}",
        loaded
            .as_ref()
            .and_then(|l| l.project_root.clone())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none detected)".into())
    );

    let ws_path = Workspace::locate(&cwd);
    outln!("workspace        {}", ws_path.display());
    match Workspace::open_at(&ws_path, &config) {
        Ok(ws) => {
            let stats = ws.capsules().stats()?;
            outln!(
                "capsules         {} ({} blobs, {:.1} MiB on disk, {:.1} MiB original)",
                stats.capsules,
                stats.blob_count,
                stats.blob_bytes as f64 / 1_048_576.0,
                stats.original_bytes as f64 / 1_048_576.0
            );
            outln!("sessions         {}", ws.events().sessions()?.len());
            outln!(
                "telemetry        local JSONL {} (never sent anywhere)",
                if config.telemetry.enabled {
                    "on"
                } else {
                    "off"
                }
            );
        }
        Err(e) => {
            outln!("workspace        FAILED: {e}");
            problems += 1;
        }
    }

    outln!("compilers        {}", ttk_compilers::registry().len());
    outln!("version          {}", env!("CARGO_PKG_VERSION"));

    // Being off PATH is the single most common reason `ttk` "does not work"
    // from another directory, so doctor names it explicitly.
    // None of these count as a problem: ttk works when called by path, so this
    // must not change doctor's exit code.
    match crate::path_env::binary_dir() {
        Ok(dir) => {
            use crate::path_env::PathPresence;
            match crate::path_env::presence(&dir) {
                PathPresence::Active => outln!("ttk on PATH      yes ({})", dir.display()),
                PathPresence::PendingNewShell => {
                    outln!(
                        "ttk on PATH      yes, but not in this shell ({})",
                        dir.display()
                    );
                    outln!(
                        "                 it is stored permanently; this terminal still has the\n\
                         \x20                environment it started with. Open a new terminal."
                    );
                }
                PathPresence::Absent => {
                    outln!("ttk on PATH      no  ({})", dir.display());
                    outln!("                 hint: `ttk install` adds it.");
                }
                PathPresence::Unknown => {
                    outln!("ttk on PATH      not in this shell ({})", dir.display());
                    outln!("                 could not read the persisted PATH.");
                }
            }
        }
        Err(e) => outln!("ttk on PATH      unknown: {e}"),
    }

    if problems == 0 {
        outln!("\nall checks passed");
    } else {
        outln!("\n{problems} problem(s) found");
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
    let out = pipeline::process(
        &ws,
        &config,
        PipelineInput::new(&content, session).command(&captured.context),
    )?;

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
        }))?;
    } else {
        if !stream {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(out.content.as_bytes())?;
            if !out.content.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
        }
        eprintln!(
            "{}",
            report::one_line(
                out.event.tokens_before(),
                out.event.tokens_after(),
                out.capsule_ref.as_deref()
            )
        );
        if captured.truncated() {
            eprintln!(
                "ttk: capture hit limits.max_capture_bytes; the capsule holds what was captured"
            );
        }
        if out.secrets_redacted > 0 {
            eprintln!(
                "ttk: {} secret(s) replaced by placeholders; real values stay local",
                out.secrets_redacted
            );
        }
    }

    // Forward the child's exit code so `ttk run` is transparent in scripts.
    Ok(captured.context.exit_code.unwrap_or(1))
}

pub fn compile(ctx: &Context, file: Option<&str>, name: Option<&str>) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
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
    let out = pipeline::process(&ws, &config, input)?;

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
        }))?;
    } else {
        out!("{}", out.content);
        if !out.content.ends_with('\n') {
            outln!();
        }
        eprintln!(
            "{}",
            report::one_line(
                out.event.tokens_before(),
                out.event.tokens_after(),
                out.capsule_ref.as_deref()
            )
        );
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// reporting
// ---------------------------------------------------------------------------

/// Lifetime totals across every recorded session in this workspace.
pub fn stats(ctx: &Context, session_limit: usize) -> Result<i32> {
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

pub fn gain(ctx: &Context, session: &str) -> Result<i32> {
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
    outln!("event        {}", event.id);
    outln!("session      {}", event.session_id);
    outln!("source       {}", event.source.as_str());
    outln!("content type {}", event.content_type.as_str());
    outln!("trust        {}", event.trust_level.as_str());
    outln!("sensitivity  {}", event.sensitivity.as_str());
    outln!("hash         {}", ttk_core::short_hash(&event.source_hash));
    outln!(
        "tokens       {} → {} (saved {})",
        event.tokens_before(),
        event.tokens_after(),
        event.tokens_saved()
    );
    if let Some(c) = &event.capsule_id {
        outln!("capsule      cap://{c}");
    }
    if !event.invariants.is_empty() {
        outln!("invariants   {} preserved", event.invariants.len());
    }
    for (k, v) in &event.metadata {
        outln!("meta.{k:<8} {v}");
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

    outln!("event {}", event.id);
    outln!(
        "detected as {} ({})",
        event.content_type.as_str(),
        event
            .metadata
            .get("detection")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
    );
    if event.transformations.is_empty() {
        outln!("\nno transformation was attempted — the content was passed through unchanged.");
        return Ok(0);
    }
    for (i, t) in event.transformations.iter().enumerate() {
        outln!("\nstep {}: {} v{}", i + 1, t.transformer, t.version);
        outln!("  tokens     {} → {}", t.tokens_before, t.tokens_after);
        outln!("  validation {:?}", t.validation);
        outln!("  input      {}", ttk_core::short_hash(&t.input_hash));
        outln!("  output     {}", ttk_core::short_hash(&t.output_hash));
        if let Some(f) = &t.fallback {
            outln!("  fallback   {}", f.summary());
            outln!("  result     original content kept");
        }
        if !t.invariants.is_empty() {
            outln!("  guaranteed {} invariant(s):", t.invariants.len());
            for inv in t.invariants.iter().take(12) {
                outln!("    [{}] {}", inv.kind.as_str(), inv.value);
            }
            if t.invariants.len() > 12 {
                outln!("    … {} more", t.invariants.len() - 12);
            }
        }
        for n in &t.notes {
            outln!("  note       {n}");
        }
    }
    if let Some(c) = &event.capsule_id {
        outln!("\nfull original: ttk retrieve {c} --level 4");
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
            outln!("{n}: {line}");
        }
        if hits.is_empty() {
            eprintln!("ttk: no match for `{needle}`");
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
    for c in &capsules {
        outln!(
            "{}  {:<14} {:>9} B  {}{}",
            c.id,
            c.content_type.as_str(),
            c.original_bytes,
            c.sensitivity.as_str(),
            if c.requires_raw_gate() {
                "  [gated]"
            } else {
                ""
            }
        );
    }
    if capsules.is_empty() {
        outln!("no capsules yet");
    }
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
    outln!("capsules        {}", s.capsules);
    outln!("blobs           {}", s.blob_count);
    outln!(
        "on disk         {:.2} MiB",
        s.blob_bytes as f64 / 1_048_576.0
    );
    outln!(
        "original size   {:.2} MiB",
        s.original_bytes as f64 / 1_048_576.0
    );
    if s.blob_bytes > 0 {
        outln!(
            "storage ratio   {:.1}x (deduplicated + compressed)",
            s.original_bytes as f64 / s.blob_bytes as f64
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
            "removed {} expired, evicted {} for size, freed {:.2} MiB",
            stats.expired_removed,
            stats.evicted_for_size,
            stats.bytes_freed as f64 / 1_048_576.0
        );
    }
    Ok(0)
}

pub fn capsule_delete(ctx: &Context, capsule: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    if ws.capsules().delete(capsule)? {
        outln!("deleted {capsule}");
        Ok(0)
    } else {
        eprintln!("ttk: no capsule matching `{capsule}`");
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
        let capsule_ref = format!("cap://{capsule_id}");
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

    outln!(
        "{:<32} {:>10} {:>10} {:>10}",
        "event",
        "before",
        "recorded",
        "now"
    );
    let (mut then_total, mut now_total) = (0u64, 0u64);
    for (id, before, then, now) in &rows {
        outln!("{id:<32} {before:>10} {then:>10} {now:>10}");
        then_total += then;
        now_total += now;
    }
    outln!("\nreplayed {} event(s)", rows.len());
    match now_total.cmp(&then_total) {
        std::cmp::Ordering::Less => outln!(
            "current compilers are {} tokens better than the recorded run",
            then_total - now_total
        ),
        std::cmp::Ordering::Greater => outln!(
            "current compilers are {} tokens worse than the recorded run",
            now_total - then_total
        ),
        std::cmp::Ordering::Equal => outln!("identical to the recorded run"),
    }
    Ok(0)
}
