//! `ttk learn`, `ttk rules` and `ttk filter`.
//!
//! The teaching side of ThanosTokenKiller. See `docs/learned-filters.md` for
//! the design; this module is the surface an agent actually touches.

use std::io::Read;
use std::path::PathBuf;

use serde_json::json;

use ttk_core::config::Config;
use ttk_core::{Error, Result};
use ttk_learn::{
    Engine, Guards, Layered, Lesson, Origin, Rule, RuleSet, Scope, annotate, project_rules_path,
    user_rules_path,
};
use ttk_store::Workspace;

use crate::commands::Context;
use crate::ui::{self, Align, errln, out, outln};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Which rule file a write goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `<workspace>/learned-filters.json` — committed with the project.
    Project,
    /// `<config>/ttk/learned-filters.json` — follows the user everywhere.
    User,
}

impl Target {
    fn origin(self) -> Origin {
        match self {
            Target::Project => Origin::Project,
            Target::User => Origin::User,
        }
    }

    fn path(self, ws: &Workspace) -> Result<PathBuf> {
        match self {
            Target::Project => Ok(project_rules_path(ws.root())),
            Target::User => user_rules_path()
                .ok_or_else(|| Error::other("this platform has no user configuration directory")),
        }
    }
}

fn read_input(file: Option<&str>) -> Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)?),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
    }
}

fn guards_from(config: &Config) -> Guards {
    Guards {
        min_literal_tokens: config.learning.min_literal_tokens as usize,
        min_literal_chars: config.learning.min_literal_chars as usize,
        max_placeholder_ratio: config.learning.max_placeholder_ratio,
        protect_errors: config.learning.protect_errors,
        learn_blocks: config.learning.learn_blocks,
        min_block_literal_chars: config.learning.min_block_literal_chars as usize,
    }
}

/// `"npm install --save"` → `("npm", Some("install"))`, path and `.exe`
/// stripped. Mirrors `CommandContext::program`/`subcommand` for a command line
/// that has already been flattened into a string on an event.
fn split_command_line(line: &str) -> (String, Option<String>) {
    let (first, rest) = match line.strip_prefix('"') {
        Some(r) => match r.split_once('"') {
            Some((prog, rest)) => (prog, rest),
            None => (r, ""),
        },
        None => match line.split_once(char::is_whitespace) {
            Some((prog, rest)) => (prog, rest),
            None => (line, ""),
        },
    };
    let program = ttk_learn::rule::normalise(first);
    let subcommand = rest
        .split_whitespace()
        .find(|a| !a.starts_with('-'))
        .map(str::to_string);
    (program, subcommand)
}

/// A short, coloured word for what a rule does with what it matches.
fn kind_cell(rule: &Rule) -> String {
    let shape = if rule.is_block() {
        format!(" x{}", rule.height())
    } else {
        String::new()
    };
    match &rule.action {
        ttk_learn::Action::Drop => ui::paint(ui::DIM, format!("drop{shape}")),
        ttk_learn::Action::Fold { .. } => ui::paint(ui::ACCENT, format!("fold{shape}")),
        ttk_learn::Action::Keep => ui::paint(ui::OK, "keep"),
    }
}

/// A pattern squeezed into one table cell.
///
/// A block rule's pattern is several lines; a table row is one. The first line
/// plus an honest count of what is not shown beats either truncating silently
/// or breaking the table.
fn pattern_cell(pattern: &str, width: usize) -> String {
    let extra = pattern.lines().count().saturating_sub(1);
    let first = pattern.lines().next().unwrap_or("");
    if extra == 0 {
        return ui::ellipsize(first, width);
    }
    format!(
        "{}  {}",
        ui::ellipsize(first, width.saturating_sub(12)),
        ui::paint(ui::DIM, format!("+{extra} line(s)"))
    )
}

/// Everything a `--capsule` / `--event` / `--last` reference gives us.
struct Source {
    capsule_ref: String,
    scope: Scope,
    /// Error-shaped lines of the stored original.
    critical_lines: Vec<String>,
}

/// Resolve the event a lesson is about, and derive its scope from the command
/// that produced it.
fn resolve_source(
    ws: &Workspace,
    capsule: Option<&str>,
    event: Option<&str>,
    last: bool,
) -> Result<Source> {
    let ev = if last {
        let session = ws.events().latest_session()?.ok_or_else(|| {
            Error::other("nothing recorded yet — run `ttk run -- <command>` first")
        })?;
        ws.events()
            .read_session(&session)?
            .into_iter()
            .rev()
            .find(|e| e.capsule_id.is_some())
            .ok_or_else(|| {
                Error::other("the newest session has no captured output to learn from")
            })?
    } else if let Some(id) = event {
        ws.events()
            .find_event(id)?
            .ok_or_else(|| Error::other(format!("no event matching `{id}`")))?
    } else {
        let needle = capsule.expect("caller checked one of the three");
        let id = needle.strip_prefix("cap://").unwrap_or(needle);
        let mut found = None;
        'outer: for session in ws.events().sessions()? {
            for e in ws.events().read_session(&session)? {
                if e.capsule_id
                    .as_ref()
                    .is_some_and(|c| c.as_str().starts_with(id))
                {
                    found = Some(e);
                    break 'outer;
                }
            }
        }
        found.ok_or_else(|| Error::other(format!("no event references capsule `{needle}`")))?
    };

    let capsule_id = ev
        .capsule_id
        .as_ref()
        .ok_or_else(|| Error::other("that event has no stored original to learn from"))?;
    let capsule_ref = format!("cap://{capsule_id}");

    let scope = match ev.metadata.get("command").and_then(|v| v.as_str()) {
        Some(line) => {
            let (program, sub) = split_command_line(line);
            match sub {
                Some(s) => Scope::command(program, s),
                None => Scope::program(program),
            }
        }
        // Piped input has no command, so only a global rule can describe it.
        None => Scope::global(),
    };

    // Only the *error-shaped* lines of the original become counter-examples.
    //
    // Every unmarked line would be wrong here: the agent annotates the
    // compiled view, so a noise line the compiler already dropped never
    // appears in the annotation and would silently block its own rule. The
    // error lines, on the other hand, are exactly the ones that must survive
    // whatever is learned — a much stronger and much narrower claim.
    let critical_lines = ws
        .capsules()
        .render(capsule_ref.as_str(), ttk_store::LEVEL_RAW, true)
        .map(|text| {
            text.lines()
                .filter(|l| ttk_learn::rule::looks_critical(l))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Ok(Source {
        capsule_ref,
        scope,
        critical_lines,
    })
}

// ---------------------------------------------------------------------------
// ttk learn
// ---------------------------------------------------------------------------

pub struct LearnArgs<'a> {
    pub file: Option<&'a str>,
    pub capsule: Option<&'a str>,
    pub event: Option<&'a str>,
    pub last: bool,
    pub scope: Option<&'a str>,
    pub global: bool,
    pub program_only: bool,
    pub user: bool,
    pub dry_run: bool,
    pub force: bool,
    pub note: Option<&'a str>,
}

pub fn learn(ctx: &Context, args: &LearnArgs<'_>) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let text = read_input(args.file)?;

    if !annotate::has_markup(&text) {
        return Err(Error::other(
            "no <filter-trash> tags found — wrap the worthless lines like this:\n\
             \x20 <filter-trash>\n\
             \x20 npm WARN deprecated glob@7.2.3: not supported\n\
             \x20 </filter-trash>\n\
             Everything you leave unmarked becomes a counter-example, so paste back\n\
             the whole output, not only the parts you want removed.",
        ));
    }
    let annotated = annotate::parse(&text);

    let source = if args.last || args.capsule.is_some() || args.event.is_some() {
        Some(resolve_source(&ws, args.capsule, args.event, args.last)?)
    } else {
        None
    };

    // Explicit --scope wins, then the widening flags, then whatever the source
    // event says, and a lesson with no evidence at all stays global.
    let mut scope = match (args.scope, &source) {
        (Some(s), _) => Scope::parse(s),
        (None, Some(src)) => src.scope.clone(),
        (None, None) => Scope::global(),
    };
    if args.global {
        scope = Scope::global();
    } else if args.program_only {
        scope = scope.widened();
    }

    let target = if args.user {
        Target::User
    } else {
        Target::Project
    };
    let path = target.path(&ws)?;
    let mut set = RuleSet::load(&path)?;

    let mut lesson = Lesson::new(&annotated, scope.clone());
    lesson.guards = guards_from(&config);
    lesson.force = args.force;
    lesson.note = args.note.map(str::to_string);
    lesson.taught_from = source.as_ref().map(|s| s.capsule_ref.clone());
    if let Some(src) = &source {
        lesson.extra_keep = src.critical_lines.clone();
    }

    let outcome = set.learn(&lesson);
    if !args.dry_run && outcome.changed() {
        set.save(&path)?;
    }

    if ctx.json {
        return ctx.print_json(json!({
            "scope": scope.to_string(),
            "target": target.origin().as_str(),
            "path": path.display().to_string(),
            "dry_run": args.dry_run,
            "created": outcome.created.iter().map(|r| json!({
                "id": r.id,
                "pattern": r.pattern,
                "examples": r.examples,
                "kind": r.action.as_str(),
                "block": r.is_block(),
                "height": r.height(),
            })).collect::<Vec<_>>(),
            "reinforced": outcome.reinforced,
            "retired": outcome.retired,
            "already_covered": outcome.already_covered,
            "generalised": outcome.generalised,
            "blocks": outcome.blocks,
            "folds": outcome.folds,
            "keeps": outcome.keeps,
            "rejected": outcome.rejected.iter().map(|r| json!({
                "line": r.line, "reason": r.reason.summary()
            })).collect::<Vec<_>>(),
            "rules_total": set.len(),
        }));
    }

    outln!(
        "{}",
        ui::banner(&format!(
            "learned from {} marked line(s)",
            annotated.trash.len()
        ))
    );
    outln!("{}", ui::kv("scope", ui::paint(ui::ACCENT, &scope)));
    outln!(
        "{}",
        ui::kv_note("rule file", path.display(), target.origin().as_str())
    );
    if annotated.unclosed {
        errln!(
            "{}",
            ui::warn(
                "an opening tag had no </filter-trash>; everything after it was treated as trash"
            )
        );
    }

    if !outcome.created.is_empty() {
        outln!("{}", ui::heading("new rules"));
        let rows: Vec<Vec<String>> = outcome
            .created
            .iter()
            .map(|r| {
                vec![
                    ui::paint(ui::OK, &r.id),
                    kind_cell(r),
                    r.examples.to_string(),
                    pattern_cell(&r.pattern, 58),
                ]
            })
            .collect();
        out!(
            "{}",
            ui::table(
                &["id", "kind", "seen", "pattern"],
                &[Align::Left, Align::Left, Align::Right, Align::Left],
                &rows
            )
        );
    }

    if !outcome.reinforced.is_empty() {
        outln!(
            "{}",
            ui::ok(format!(
                "{} rule(s) reinforced by this lesson",
                outcome.reinforced.len()
            ))
        );
    }
    if !outcome.retired.is_empty() {
        outln!("{}", ui::heading("retired"));
        for id in &outcome.retired {
            outln!(
                "  {} {}",
                ui::paint(ui::WARN, id),
                ui::paint(ui::DIM, "disabled — it matched a <filter-keep> line")
            );
        }
    }
    if outcome.generalised > 0 {
        outln!(
            "{}",
            ui::ok(format!(
                "{} line shape(s) folded into a wider pattern",
                outcome.generalised
            ))
        );
    }
    if outcome.folds > 0 {
        outln!(
            "{}",
            ui::ok(format!(
                "{} fold rule(s): the run is replaced by its caption, not deleted",
                outcome.folds
            ))
        );
    }
    if outcome.keeps > 0 {
        outln!(
            "{}",
            ui::warn(format!(
                "{} whitelist rule(s): from now on this command keeps only what a keep                  rule names — errors always survive",
                outcome.keeps
            ))
        );
    }
    if outcome.blocks > 0 {
        outln!(
            "{}",
            ui::ok(format!(
                "{} run(s) learned as a block: no single line of them carried \
                 enough signal to be a rule on its own",
                outcome.blocks
            ))
        );
    }
    if outcome.already_covered > 0 {
        outln!(
            "  {}",
            ui::paint(
                ui::DIM,
                format!(
                    "{} marked line(s) were already covered by an existing rule",
                    outcome.already_covered
                )
            )
        );
    }

    if !outcome.rejected.is_empty() {
        outln!("{}", ui::heading("not learned"));
        for r in &outcome.rejected {
            outln!("{}", ui::warn(ui::ellipsize(r.line.trim(), 72)));
            outln!("{}", ui::detail(r.reason.summary()));
        }
    }

    outln!();
    if args.dry_run {
        outln!(
            "{}",
            ui::paint(ui::DIM, "dry run: nothing was written to the rule file")
        );
    } else if !outcome.changed() {
        outln!("{}", ui::paint(ui::DIM, "nothing new to learn"));
    } else {
        outln!(
            "{}",
            ui::ok(match target {
                Target::Project => format!("{} rule(s) in force for this project", set.len()),
                Target::User => format!(
                    "{} global rule(s) in force for every project (global folder)",
                    set.len()
                ),
            })
        );
        outln!("{}", ui::hint("see them with ", "ttk rules"));
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// ttk rules
// ---------------------------------------------------------------------------

fn rule_row(rule: &Rule, origin: Option<Origin>, estimated: bool) -> Vec<String> {
    let state = if rule.enabled {
        ui::paint(ui::OK, &rule.id)
    } else {
        ui::paint(ui::DIM, &rule.id)
    };
    vec![
        state,
        rule.scope.to_string(),
        kind_cell(rule),
        ui::tokens(rule.tokens_saved, estimated),
        rule.lines_removed.to_string(),
        rule.hits.to_string(),
        origin.map(|o| o.as_str().to_string()).unwrap_or_default(),
        pattern_cell(&rule.pattern, 56),
    ]
}

pub fn rules_list(ctx: &Context, all: bool, limit: usize) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;

    let mut rules: Vec<&Rule> = layered
        .merged
        .rules
        .iter()
        .filter(|r| all || r.enabled)
        .collect();
    rules.sort_by_key(|r| (std::cmp::Reverse(r.tokens_saved), r.id.clone()));
    if limit > 0 {
        rules.truncate(limit);
    }

    if ctx.json {
        return ctx.print_json(json!(
            rules
                .iter()
                .map(|r| {
                    let mut v = serde_json::to_value(r).unwrap_or(json!({}));
                    if let Some(o) = layered.origin_of(&r.id) {
                        v["origin"] = json!(o.as_str());
                    }
                    v
                })
                .collect::<Vec<_>>()
        ));
    }

    outln!("{}", ui::banner("learned filters"));
    if layered.is_empty() {
        outln!();
        outln!(
            "{}",
            ui::paint(
                ui::DIM,
                "nothing learned yet. Mark the junk in a command's output:"
            )
        );
        outln!();
        outln!("  {}", ui::paint(ui::CODE, "<filter-trash>"));
        outln!(
            "  {}",
            ui::paint(ui::DIM, "npm WARN deprecated glob@7.2.3: not supported")
        );
        outln!("  {}", ui::paint(ui::CODE, "</filter-trash>"));
        outln!();
        outln!(
            "{}",
            ui::hint("then feed it back", "ttk learn --last < annotated.txt")
        );
        return Ok(0);
    }

    let disabled = layered.merged.rules.iter().filter(|r| !r.enabled).count();
    let blocks = layered.merged.rules.iter().filter(|r| r.is_block()).count();
    let saved: u64 = layered.merged.rules.iter().map(|r| r.tokens_saved).sum();
    let removed: u64 = layered.merged.rules.iter().map(|r| r.lines_removed).sum();
    let mut aside = Vec::new();
    if blocks > 0 {
        aside.push(format!("{blocks} match a run of lines"));
    }
    if disabled > 0 {
        aside.push(format!("{disabled} disabled"));
    }
    outln!(
        "{}",
        ui::kv_note("rules", layered.merged.len(), aside.join(", "))
    );
    outln!(
        "{}",
        ui::kv_num("tokens saved", ui::tokens(saved, true), "")
    );
    outln!(
        "{}",
        ui::kv_num("lines removed", ui::thousands(removed), "")
    );

    outln!("{}", ui::heading("ranked by what they actually saved"));
    let rows: Vec<Vec<String>> = rules
        .iter()
        .map(|r| rule_row(r, layered.origin_of(&r.id), true))
        .collect();
    out!(
        "{}",
        ui::table(
            &[
                "id", "scope", "does", "saved", "lines", "hits", "where", "pattern"
            ],
            &[
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
                Align::Left
            ],
            &rows
        )
    );
    if !all && disabled > 0 {
        outln!("{}", ui::hint("disabled rules", "ttk rules list --all"));
    }
    Ok(0)
}

pub fn rules_show(ctx: &Context, id: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    let rule = layered.merged.resolve(id)?;

    if ctx.json {
        return ctx.print_json(serde_json::to_value(rule).map_err(Error::other)?);
    }

    outln!("{}", ui::banner("rule"));
    outln!("{}", ui::kv("id", ui::paint(ui::ACCENT, &rule.id)));
    outln!("{}", ui::kv("scope", &rule.scope));
    outln!(
        "{}",
        ui::kv(
            "kind",
            match &rule.action {
                ttk_learn::Action::Drop if rule.is_block() => ui::paint(
                    ui::ACCENT,
                    format!("block — removes {} consecutive lines", rule.height()),
                ),
                ttk_learn::Action::Drop => ui::paint(ui::DIM, "line — removes what it matches"),
                ttk_learn::Action::Fold { summary } => ui::paint(
                    ui::ACCENT,
                    format!("fold — the run becomes `[folded] {summary} (N line(s))`"),
                ),
                ttk_learn::Action::Keep => ui::paint(
                    ui::OK,
                    "keep — this survives, and whatever no keep rule names does not",
                ),
            }
        )
    );
    outln!(
        "{}",
        ui::kv_multiline("pattern", &ui::paint(ui::CODE, &rule.pattern))
    );
    outln!(
        "{}",
        ui::kv(
            "state",
            if rule.enabled {
                ui::paint(ui::OK, "enabled")
            } else {
                ui::paint(ui::WARN, "disabled")
            }
        )
    );
    outln!(
        "{}",
        ui::kv(
            "where",
            layered
                .origin_of(&rule.id)
                .map(|o| o.as_str())
                .unwrap_or("(merged)")
        )
    );
    outln!(
        "{}",
        ui::kv("taught from", rule.examples.to_string() + " example(s)")
    );
    if let Some(c) = &rule.taught_from {
        outln!("{}", ui::kv("capsule", c));
    }
    if let Some(n) = &rule.note {
        outln!("{}", ui::kv("note", n));
    }
    if rule.forced {
        outln!(
            "{}",
            ui::warn("learned with --force from a line that looks like an error")
        );
    }
    outln!("{}", ui::heading("what it has done"));
    outln!("{}", ui::kv_num("hits", rule.hits, "runs it fired in"));
    outln!("{}", ui::kv_num("lines removed", rule.lines_removed, ""));
    outln!(
        "{}",
        ui::kv_num("tokens saved", ui::tokens(rule.tokens_saved, true), "")
    );
    if rule.hits == 0 {
        outln!(
            "{}",
            ui::paint(
                ui::DIM,
                "  never fired yet — `ttk rules prune` retires rules like this"
            )
        );
    }

    outln!("{}", ui::heading("how to read the pattern"));
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "a bare word must match exactly; {n} {ver} {hex} {t} {size} {path} {url} {uuid}"
        )
    );
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "match any value of that shape, and {*} matches any single token."
        )
    );
    if rule.is_block() {
        outln!(
            "  {}",
            ui::paint(
                ui::DIM,
                "every line has to match, in this order and next to each other; the run is"
            )
        );
        outln!(
            "  {}",
            ui::paint(
                ui::DIM,
                "removed as a whole or not at all. Blank lines inside it go with it."
            )
        );
    }
    Ok(0)
}

/// Flip `enabled` on one rule, in whichever file it lives in.
pub fn rules_set_enabled(ctx: &Context, id: &str, enabled: bool) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    let rule = layered.merged.resolve(id)?;
    let full_id = rule.id.clone();
    let origin = layered
        .origin_of(&full_id)
        .ok_or_else(|| Error::other("that rule is not in a writable file"))?;
    let target = match origin {
        Origin::Project => Target::Project,
        Origin::User => Target::User,
    };
    let path = target.path(&ws)?;

    let mut set = RuleSet::load(&path)?;
    let Some(r) = set.get_mut(&full_id) else {
        return Err(Error::other(format!(
            "`{full_id}` is not in {}",
            path.display()
        )));
    };
    let was = r.enabled;
    r.enabled = enabled;
    set.save(&path)?;

    if ctx.json {
        return ctx
            .print_json(json!({"id": full_id, "enabled": enabled, "changed": was != enabled}));
    }
    let word = if enabled { "enabled" } else { "disabled" };
    if was == enabled {
        outln!(
            "{}",
            ui::paint(ui::DIM, format!("{full_id} was already {word}"))
        );
    } else {
        outln!("{}", ui::ok(format!("{full_id} {word}")));
    }
    Ok(0)
}

pub fn rules_forget(ctx: &Context, id: &str) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    let full_id = layered.merged.resolve(id)?.id.clone();

    let mut removed_from = Vec::new();
    for target in [Target::Project, Target::User] {
        let Ok(path) = target.path(&ws) else { continue };
        let mut set = RuleSet::load(&path)?;
        if set.remove(&full_id).is_some() {
            set.save(&path)?;
            removed_from.push(target.origin().as_str());
        }
    }

    if ctx.json {
        return ctx.print_json(json!({"id": full_id, "removed_from": removed_from}));
    }
    if removed_from.is_empty() {
        errln!(
            "{}",
            ui::bad(format!("{full_id} was not in any writable rule file"))
        );
        return Ok(1);
    }
    outln!(
        "{}",
        ui::ok(format!("{full_id} forgotten ({})", removed_from.join(", ")))
    );
    outln!(
        "{}",
        ui::paint(
            ui::DIM,
            "teach it again any time — or use `ttk rules disable` to keep the record."
        )
    );
    Ok(0)
}

pub fn rules_prune(ctx: &Context, unused_days: u64, dry_run: bool) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let now = ttk_core::ids::now_millis();
    let mut dropped = Vec::new();

    for target in [Target::Project, Target::User] {
        let Ok(path) = target.path(&ws) else { continue };
        let mut set = RuleSet::load(&path)?;
        if set.is_empty() {
            continue;
        }
        let gone = set.prune(unused_days, now);
        if !gone.is_empty() && !dry_run {
            set.save(&path)?;
        }
        dropped.extend(gone);
    }

    if ctx.json {
        return ctx.print_json(json!({
            "dropped": dropped.iter().map(|r| json!({"id": r.id, "pattern": r.pattern}))
                .collect::<Vec<_>>(),
            "dry_run": dry_run,
        }));
    }
    if dropped.is_empty() {
        outln!(
            "{}",
            ui::ok(format!("no rule has been idle for {unused_days} day(s)"))
        );
        return Ok(0);
    }
    outln!(
        "{}",
        ui::banner(&format!("{} rule(s) never fired", dropped.len()))
    );
    for r in &dropped {
        outln!(
            "  {}  {}",
            ui::paint(ui::DIM, &r.id),
            ui::ellipsize(&r.pattern, 66)
        );
    }
    outln!();
    if dry_run {
        outln!("{}", ui::paint(ui::DIM, "dry run: nothing was removed"));
    } else {
        outln!("{}", ui::ok("removed"));
    }
    Ok(0)
}

pub fn rules_export(ctx: &Context) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    // Export is always the machine readable form: it is meant to be piped into
    // `ttk rules import` on another machine, not read.
    outln!("{}", layered.merged.to_json());
    Ok(0)
}

pub fn rules_import(ctx: &Context, file: Option<&str>, user: bool) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let incoming = RuleSet::from_json(&read_input(file)?)?;
    let target = if user { Target::User } else { Target::Project };
    let path = target.path(&ws)?;

    let mut set = RuleSet::load(&path)?;
    let before = set.len();
    let added = set.merge(incoming);
    set.save(&path)?;

    if ctx.json {
        return ctx.print_json(json!({
            "added": added,
            "total": set.len(),
            "path": path.display().to_string(),
        }));
    }
    outln!(
        "{}",
        ui::ok(format!(
            "{added} new rule(s), {} already known, {} in force",
            set.len().saturating_sub(before + added),
            set.len()
        ))
    );
    outln!("{}", ui::kv("rule file", path.display()));
    Ok(0)
}

// ---------------------------------------------------------------------------
// ttk suggest
// ---------------------------------------------------------------------------

pub fn suggest(
    ctx: &Context,
    scope: Option<&str>,
    min_runs: u64,
    limit: usize,
    lesson: bool,
) -> Result<i32> {
    let (_, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    let rules: Vec<&Rule> = layered.merged.rules.iter().collect();
    let want = scope.map(Scope::parse);
    let found = crate::suggest::scan(&ws, &rules, min_runs, want.as_ref())?;

    let mut items = found.items.clone();
    if limit > 0 {
        items.truncate(limit);
    }

    if ctx.json {
        return ctx.print_json(json!({
            "events_scanned": found.events_scanned,
            "suggestions": items.iter().map(|s| json!({
                "scope": s.scope.to_string(),
                "pattern": s.pattern,
                "example": s.example,
                "runs": s.runs,
                "lines": s.lines,
                "tokens": s.tokens,
            })).collect::<Vec<_>>(),
        }));
    }

    // `--lesson` prints nothing but the draft, so it can be piped straight
    // into `ttk learn` after a human has looked at it.
    if lesson {
        let scope =
            want.unwrap_or_else(|| items.first().map(|s| s.scope.clone()).unwrap_or_default());
        let same: Vec<crate::suggest::Suggestion> =
            items.iter().filter(|s| s.scope == scope).cloned().collect();
        out!("{}", crate::suggest::to_lesson(&same, &scope));
        return Ok(0);
    }

    outln!(
        "{}",
        ui::banner(&format!(
            "noise that keeps coming back · {} run(s) examined",
            found.events_scanned
        ))
    );
    if items.is_empty() {
        outln!();
        outln!(
            "{}",
            ui::paint(
                ui::DIM,
                "nothing repeats often enough to be worth a rule yet. This reads the\n\
                 originals ttk already stored, so it gets better the more you run."
            )
        );
        return Ok(0);
    }

    outln!(
        "{}",
        ui::paint(
            ui::DIM,
            "  Nothing below has been learned. Error-shaped lines were never considered."
        )
    );
    outln!(
        "{}",
        ui::heading("candidates, by what removing them would save")
    );
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|s| {
            let runs_of_scope = found
                .runs_by_scope
                .get(&s.scope.to_string())
                .copied()
                .unwrap_or(0);
            vec![
                s.scope.to_string(),
                ui::paint(ui::OK, ui::tokens(s.tokens, true)),
                ui::thousands(s.lines),
                format!(
                    "{}/{}  {:.0}%",
                    s.runs,
                    runs_of_scope,
                    s.share(runs_of_scope) * 100.0
                ),
                ui::ellipsize(&s.example, 56),
            ]
        })
        .collect();
    out!(
        "{}",
        ui::table(
            &["scope", "saves", "lines", "runs", "example"],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left
            ],
            &rows
        )
    );

    outln!();
    let scope_hint = items
        .first()
        .map(|s| s.scope.to_string())
        .unwrap_or_else(|| "global".into());
    outln!(
        "{}",
        ui::hint(
            "review the draft",
            &format!("ttk suggest --lesson --scope \"{scope_hint}\"")
        )
    );
    outln!(
        "{}",
        ui::hint(
            "then teach it  ",
            &format!(
                "ttk suggest --lesson --scope \"{scope_hint}\" | ttk learn --scope \"{scope_hint}\""
            )
        )
    );
    outln!(
        "  {}",
        ui::paint(
            ui::DIM,
            "read the draft before you pipe it: deleting a line from it is how you\n\
             say `keep that one`, and it is the only review this feature gets."
        )
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// ttk filter — dry run the engine without learning or storing anything
// ---------------------------------------------------------------------------

pub fn filter(
    ctx: &Context,
    file: Option<&str>,
    scope: Option<&str>,
    explain: bool,
) -> Result<i32> {
    let (config, ws) = ctx.workspace()?;
    let layered = Layered::load(ws.root())?;
    let text = read_input(file)?;

    let scope = scope.map(Scope::parse).unwrap_or_default();
    let engine = Engine::for_command(
        &layered.merged,
        scope.program.as_deref(),
        scope.subcommand.as_deref(),
    )
    .protect_errors(config.learning.protect_errors);
    let pass = engine.filter(&text);

    if ctx.json {
        return ctx.print_json(json!({
            "content": pass.text,
            "rules_considered": engine.len(),
            "removed_lines": pass.removed_lines,
            "protected_lines": pass.protected_lines,
            "blank_lines_collapsed": pass.blank_lines_collapsed,
            "folded_runs": pass.folded_runs,
            "whitelisted_away": pass.whitelisted_away,
            "tokens_before": pass.tokens_before.value,
            "tokens_after": pass.tokens_after.value,
            "by_rule": pass.by_rule.iter().map(|h| json!({
                "id": h.id, "pattern": h.pattern, "lines": h.lines, "tokens": h.tokens, "runs": h.runs
            })).collect::<Vec<_>>(),
        }));
    }

    if !explain {
        out!("{}", pass.text);
        if !pass.text.ends_with('\n') {
            outln!();
        }
    }

    errln!(
        "{}",
        ui::banner(&format!(
            "filter dry run · {} rule(s) in scope, {} matching a run",
            engine.len(),
            engine.blocks()
        ))
    );
    errln!("{}", ui::kv("scope", scope.to_string()));
    errln!(
        "{}",
        ui::kv_num(
            "tokens",
            format!(
                "{} {} {}",
                ui::tokens(pass.tokens_before.value, true),
                ui::glyphs().arrow,
                ui::tokens(pass.tokens_after.value, true)
            ),
            ""
        )
    );
    errln!("{}", ui::kv_num("lines removed", pass.removed_lines, ""));
    if pass.folded_runs > 0 {
        errln!(
            "{}",
            ui::kv_num("runs folded", pass.folded_runs, "replaced by a caption")
        );
    }
    if pass.whitelisted_away > 0 {
        errln!(
            "{}",
            ui::kv_num(
                "not whitelisted",
                pass.whitelisted_away,
                "no keep rule named them"
            )
        );
    }
    if pass.protected_lines > 0 {
        errln!(
            "{}",
            ui::warn(format!(
                "{} matched line(s) kept anyway because they look like errors",
                pass.protected_lines
            ))
        );
    }
    if !pass.by_rule.is_empty() {
        errln!("{}", ui::heading("which rules fired"));
        let rows: Vec<Vec<String>> = pass
            .by_rule
            .iter()
            .map(|h| {
                vec![
                    ui::paint(ui::OK, &h.id),
                    h.runs.to_string(),
                    h.lines.to_string(),
                    ui::tokens(h.tokens, true),
                    pattern_cell(&h.pattern, 56),
                ]
            })
            .collect();
        let text = ui::table(
            &["id", "runs", "lines", "tokens", "pattern"],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
            ],
            &rows,
        );
        for line in text.lines() {
            errln!("{line}");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_pattern_fits_one_table_cell_without_hiding_its_height() {
        let one = pattern_cell("npm WARN deprecated {ver}", 40);
        assert_eq!(one, "npm WARN deprecated {ver}");

        let many = pattern_cell("=== top ===\n  middle\n=== bottom ===", 40);
        assert!(many.starts_with("=== top ==="), "{many}");
        assert!(many.contains("+2 line(s)"), "{many}");
        assert!(!many.contains('\n'), "a table row is one line");
    }

    #[test]
    fn command_lines_split_into_a_scope() {
        assert_eq!(
            split_command_line("npm install --save-dev"),
            ("npm".to_string(), Some("install".to_string()))
        );
        assert_eq!(
            split_command_line(r#""C:\Program Files\nodejs\NPM.EXE" ci"#),
            ("npm".to_string(), Some("ci".to_string()))
        );
        assert_eq!(
            split_command_line("/usr/bin/cargo"),
            ("cargo".to_string(), None)
        );
        // Flags never become a subcommand.
        assert_eq!(
            split_command_line("pytest -q"),
            ("pytest".to_string(), None)
        );
    }

    #[test]
    fn guards_follow_the_configuration() {
        let mut config = Config::default();
        config.learning.min_literal_tokens = 5;
        config.learning.protect_errors = false;
        let g = guards_from(&config);
        assert_eq!(g.min_literal_tokens, 5);
        assert!(!g.protect_errors);
    }
}
