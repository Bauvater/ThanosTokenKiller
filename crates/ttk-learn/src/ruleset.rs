//! The rule file: loading, merging, learning, retiring.
//!
//! Rules live in plain, pretty printed JSON so a project's accumulated
//! knowledge is greppable, diffable and reviewable in a pull request. That
//! file — not the binary — is the artefact this feature produces.
//!
//! ```text
//! .ttk/learned-filters.json          project rules, meant to be committed
//! <config>/ttk/learned-filters.json  user rules, every project you work on
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ttk_core::{Error, Result};

use crate::annotate::Annotated;
use crate::pattern::{MAX_BLOCK_LINES, Pattern, Template};
use crate::rule::{Action, Guards, Rejection, Rule, Scope, rule_id_for};

/// Bumped whenever the on-disk shape changes incompatibly.
pub const RULES_SCHEMA_VERSION: u32 = 1;

/// File name used in both the project workspace and the user config directory.
pub const RULES_FILE: &str = "learned-filters.json";

/// Upper bound on distinct templates considered in a single lesson.
///
/// Merging is quadratic in this number, and a lesson that marks more than a few
/// hundred distinct line shapes is pasting a whole log rather than teaching.
const MAX_TEMPLATES_PER_LESSON: usize = 256;

/// Where a rule set was read from. Kept so `ttk rules` can say which file a
/// rule lives in and writes go back to the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Project,
    User,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Project => "project",
            Origin::User => "user",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSet {
    pub schema_version: u32,
    /// Ordered by insertion; the id is the identity.
    pub rules: Vec<Rule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self {
            schema_version: RULES_SCHEMA_VERSION,
            rules: Vec::new(),
        }
    }
}

/// One line of a lesson that could not become a rule, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedLine {
    pub line: String,
    pub reason: Rejection,
}

/// What [`RuleSet::learn`] did.
#[derive(Debug, Clone, Default)]
pub struct LearnOutcome {
    /// Rules that did not exist before.
    pub created: Vec<Rule>,
    /// Existing rules that gained an example, by id.
    pub reinforced: Vec<String>,
    /// Trash lines already covered by a rule that was in the set beforehand.
    pub already_covered: u64,
    pub rejected: Vec<RejectedLine>,
    /// Distinct line shapes that were folded into a wider pattern.
    pub generalised: u64,
    /// Runs that became a block rule because no line rule could describe them.
    pub blocks: u64,
    /// Rules that replace their match with a one line summary.
    pub folds: u64,
    /// Rules that name what to keep, so everything else can go.
    pub keeps: u64,
    /// Rules disabled because a `<filter-keep>` line proved them wrong.
    pub retired: Vec<String>,
}

impl LearnOutcome {
    pub fn changed(&self) -> bool {
        !self.created.is_empty() || !self.reinforced.is_empty() || !self.retired.is_empty()
    }
}

/// Everything needed to turn one annotation into rules.
#[derive(Debug, Clone)]
pub struct Lesson<'a> {
    pub annotated: &'a Annotated,
    pub scope: Scope,
    /// Extra counter-examples beyond the unmarked text — typically every
    /// unmarked line of the capsule the lesson came from.
    pub extra_keep: Vec<String>,
    pub guards: Guards,
    /// Learn even from error-shaped lines.
    pub force: bool,
    pub note: Option<String>,
    pub taught_from: Option<String>,
}

impl<'a> Lesson<'a> {
    pub fn new(annotated: &'a Annotated, scope: Scope) -> Self {
        Self {
            annotated,
            scope,
            extra_keep: Vec::new(),
            guards: Guards::default(),
            force: false,
            note: None,
            taught_from: None,
        }
    }
}

impl RuleSet {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// Resolve an id or a unique prefix of one.
    pub fn resolve(&self, id_or_prefix: &str) -> Result<&Rule> {
        let needle = id_or_prefix.trim();
        if let Some(exact) = self.get(needle) {
            return Ok(exact);
        }
        let mut hits = self.rules.iter().filter(|r| r.id.starts_with(needle));
        match (hits.next(), hits.next()) {
            (Some(r), None) => Ok(r),
            (Some(_), Some(_)) => Err(Error::other(format!(
                "`{needle}` matches more than one rule — use the full id"
            ))),
            _ => Err(Error::other(format!("no rule matching `{needle}`"))),
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|r| r.id == id)
    }

    /// Insert, or absorb into the rule that already has this id.
    pub fn upsert(&mut self, rule: Rule) -> bool {
        match self.get_mut(&rule.id) {
            Some(existing) => {
                existing.absorb(&rule);
                false
            }
            None => {
                self.rules.push(rule);
                true
            }
        }
    }

    /// Fold `other` into `self`. Used to layer user rules under project rules
    /// and to implement `ttk rules import`.
    pub fn merge(&mut self, other: RuleSet) -> usize {
        let mut added = 0;
        for rule in other.rules {
            if self.upsert(rule) {
                added += 1;
            }
        }
        added
    }

    pub fn remove(&mut self, id: &str) -> Option<Rule> {
        let i = self.rules.iter().position(|r| r.id == id)?;
        Some(self.rules.remove(i))
    }

    /// Rules that could fire for output of `program` / `subcommand`.
    pub fn applicable(&self, program: Option<&str>, subcommand: Option<&str>) -> Vec<&Rule> {
        self.rules
            .iter()
            .filter(|r| r.enabled && r.scope.applies_to(program, subcommand))
            .collect()
    }

    /// Disable every rule that could have removed one of `lines`.
    ///
    /// This is the undo for a rule that turned out to remove something useful:
    /// mark the line with `<filter-keep>` and the rule that ate it retires.
    /// Rules are disabled rather than deleted so the mistake stays auditable.
    ///
    /// For a block rule this asks whether *any* of its lines matches, which
    /// over-approximates: a block might have that shape somewhere and still not
    /// have fired here. Retiring the wrong rule costs one `ttk rules enable`;
    /// leaving the right one in place costs the evidence again.
    pub fn retire_matching(&mut self, lines: &[String]) -> Vec<String> {
        let mut retired = Vec::new();
        for rule in &mut self.rules {
            if rule.enabled && lines.iter().any(|l| rule.template.any_line_matches(l)) {
                rule.enabled = false;
                retired.push(rule.id.clone());
            }
        }
        retired
    }

    /// Drop rules that have never fired and are older than `days`.
    pub fn prune(&mut self, days: u64, now_millis: u64) -> Vec<Rule> {
        let cutoff = now_millis.saturating_sub(days * 86_400_000);
        let mut dropped = Vec::new();
        self.rules.retain(|r| {
            let stale = r.hits == 0 && r.created_millis < cutoff;
            if stale {
                dropped.push(r.clone());
            }
            !stale
        });
        dropped
    }

    /// Turn one annotation into rules.
    ///
    /// Two passes, in this order and for a reason:
    ///
    /// 1. **Line rules**, one per distinct line shape, generalised where the
    ///    evidence supports it. This is what handles ordinary repeated noise.
    /// 2. **Block rules** for the contiguous runs that pass 1 could not
    ///    describe — the lines it refused as too general. A banner is exactly
    ///    that: twelve lines of decoration, each meaningless alone.
    ///
    /// Because pass 2 only ever looks at what pass 1 left behind, the two never
    /// compete for the same lines, and a block rule is never the easy way to
    /// get around a guard — it is the shape that lets a guard say yes.
    pub fn learn(&mut self, lesson: &Lesson<'_>) -> LearnOutcome {
        let mut outcome = LearnOutcome::default();

        // Every unmarked line is a counter-example, and so is anything the
        // caller supplied from the stored original.
        //
        // Document order is preserved and duplicates are kept. That looks
        // wasteful and is not: a block rule is checked against every *window*
        // of the kept lines, so sorting them would hide exactly the runs a
        // block must not eat, and deduplicating would delete the repeated
        // decoration lines that make a banner a banner.
        let mut keep: Vec<String> = lesson.annotated.keep.clone();
        keep.extend(lesson.extra_keep.iter().cloned());

        // A `<filter-keep>` line is an instruction, not merely evidence:
        // whatever removed it was wrong, so retire it before learning anything
        // new. Doing this first means a lesson can correct and re-teach in one
        // step.
        outcome.retired = self.retire_matching(&lesson.annotated.explicit_keep);

        // Marked lines an existing rule already removes are not new evidence.
        let covered_before = self.coverage(lesson);
        outcome.already_covered = covered_before.iter().filter(|c| **c).count() as u64;

        // -- pass 1: line rules -------------------------------------------
        let fresh: Vec<&String> = lesson
            .annotated
            .runs
            .iter()
            .flatten()
            .zip(&covered_before)
            .filter(|(_, covered)| !**covered)
            .map(|(line, _)| line)
            .collect();

        let mut groups: BTreeMap<String, (Template, String, u32)> = BTreeMap::new();
        let mut rejected: BTreeMap<String, Rejection> = BTreeMap::new();
        for line in fresh {
            match Template::from_line(line) {
                Some(t) => {
                    let entry = groups
                        .entry(t.canonical())
                        .or_insert_with(|| (t, line.clone(), 0));
                    entry.2 += 1;
                }
                None => {
                    rejected.insert(line.clone(), Rejection::Unusable);
                }
            }
        }
        if groups.len() > MAX_TEMPLATES_PER_LESSON {
            // Keep the shapes seen most often; a lesson this wide is a paste,
            // not a teaching, and the repeated shapes are the real noise.
            let mut by_count: Vec<_> = groups.into_iter().collect();
            by_count.sort_by_key(|(_, (_, _, n))| std::cmp::Reverse(*n));
            by_count.truncate(MAX_TEMPLATES_PER_LESSON);
            groups = by_count.into_iter().collect();
        }

        let mut candidates: Vec<(Template, String, u32)> = groups.into_values().collect();
        outcome.generalised = self.generalise(&mut candidates, &keep, lesson);
        let mut outcome_blocks = LearnOutcome::default();

        for (template, source_line, examples) in candidates {
            let pattern = Pattern::line(template);
            let source = [source_line.clone()];
            if let Err(reason) =
                crate::rule::check(&pattern, &source, &keep, &lesson.guards, lesson.force)
            {
                rejected.insert(source_line, reason);
                continue;
            }
            self.install(lesson, pattern, examples, &source_line, &mut outcome);
        }

        // -- pass 2: block rules for what is still uncovered ---------------
        if lesson.guards.learn_blocks {
            outcome.blocks = self.learn_blocks(lesson, &keep, &mut rejected, &mut outcome_blocks);
            outcome.created.append(&mut outcome_blocks.created);
            outcome.reinforced.append(&mut outcome_blocks.reinforced);
        } else {
            for run in &lesson.annotated.runs {
                if run.len() < 2 {
                    continue;
                }
                for line in run {
                    if let Some(slot) = rejected.get_mut(line)
                        && slot.is_generality()
                    {
                        *slot = Rejection::BlocksDisabled;
                    }
                }
            }
        }

        // -- pass 3: folds and whitelists ---------------------------------
        //
        // Both are taught with their own tag, so neither competes with the
        // passes above: a folded run was never offered to them, and a
        // whitelist line is not something to remove at all.
        let mut extra = LearnOutcome::default();
        outcome.folds = self.learn_folds(lesson, &keep, &mut rejected, &mut extra);
        outcome.keeps = self.learn_keeps(lesson, &mut rejected, &mut extra);
        outcome.created.append(&mut extra.created);
        outcome.reinforced.append(&mut extra.reinforced);

        // A rejection only stands if nothing ended up covering the line. A run
        // rescued by a block rule must not also be reported as refused.
        let covered_after = self.coverage(lesson);
        let still_uncovered: std::collections::BTreeSet<&String> = lesson
            .annotated
            .runs
            .iter()
            .flatten()
            .zip(&covered_after)
            .filter(|(_, covered)| !**covered)
            .map(|(line, _)| line)
            .collect();
        // Only a rejection about a *trash* line can be overtaken this way. A
        // fold or a whitelist line is not in `runs` at all, so asking whether a
        // drop rule covers it would silently swallow its refusal.
        let in_a_run: std::collections::BTreeSet<&String> =
            lesson.annotated.runs.iter().flatten().collect();
        outcome.rejected = rejected
            .into_iter()
            .filter(|(line, _)| !in_a_run.contains(line) || still_uncovered.contains(line))
            .map(|(line, reason)| RejectedLine { line, reason })
            .collect();

        outcome
    }

    /// Create the rule, or reinforce the one that already has this id.
    fn install(
        &mut self,
        lesson: &Lesson<'_>,
        pattern: Pattern,
        examples: u32,
        source_line: &str,
        outcome: &mut LearnOutcome,
    ) {
        self.install_action(
            lesson,
            pattern,
            Action::Drop,
            examples,
            source_line,
            outcome,
        );
    }

    fn install_action(
        &mut self,
        lesson: &Lesson<'_>,
        pattern: Pattern,
        action: Action,
        examples: u32,
        source_line: &str,
        outcome: &mut LearnOutcome,
    ) {
        let id = rule_id_for(&lesson.scope, &pattern, &action);
        if let Some(existing) = self.get_mut(&id) {
            existing.examples = existing.examples.saturating_add(examples);
            // Teaching a disabled rule again is an explicit request for it.
            existing.enabled = true;
            outcome.reinforced.push(id);
            return;
        }
        let mut rule = Rule::with_action(lesson.scope.clone(), pattern, action);
        rule.examples = examples;
        rule.note = lesson.note.clone();
        rule.taught_from = lesson.taught_from.clone();
        rule.forced = lesson.force && crate::rule::looks_critical(source_line);
        outcome.created.push(rule.clone());
        self.rules.push(rule);
    }

    /// For every marked line of the lesson, in order: does a rule remove it?
    ///
    /// Delegates to [`crate::engine::covered_lines`], so "covered" means
    /// exactly what the filter will actually do — blocks, error guard and all.
    fn coverage(&self, lesson: &Lesson<'_>) -> Vec<bool> {
        let rules: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| {
                r.enabled
                    && r.scope.applies_to(
                        lesson.scope.program.as_deref(),
                        lesson.scope.subcommand.as_deref(),
                    )
            })
            .collect();
        let protect = lesson.guards.protect_errors && !lesson.force;
        // Run by run: a rule may not match across a boundary that the
        // annotation says is not contiguous in the first place.
        lesson
            .annotated
            .runs
            .iter()
            .flat_map(|run| {
                let lines: Vec<&str> = run.iter().map(String::as_str).collect();
                crate::engine::covered_lines(&rules, &lines, protect)
            })
            .collect()
    }

    /// Propose one block rule per uncovered stretch of a run.
    fn learn_blocks(
        &mut self,
        lesson: &Lesson<'_>,
        keep: &[String],
        rejected: &mut BTreeMap<String, Rejection>,
        outcome: &mut LearnOutcome,
    ) -> u64 {
        let mut made = 0;
        let mut offset = 0;
        let coverage = self.coverage(lesson);
        // `runs` and `coverage` are the same sequence, so one cursor walks both.
        let runs: Vec<Vec<String>> = lesson.annotated.runs.clone();

        for run in runs {
            let cov = &coverage[offset..offset + run.len()];
            offset += run.len();

            if run.len() < 2 {
                continue;
            }
            // A block is offered for the *whole* run, not for the gaps in it.
            //
            // Real banners are not uniformly unlearnable: a box drawn around
            // one readable title line leaves that title perfectly learnable on
            // its own, and describing only the leftovers would turn one banner
            // into two rules that each match half of it. Describing the run is
            // what produces one rule per banner.
            //
            // The majority test is what stops the opposite mistake. Forty npm
            // warnings that one line rule already handles must not also become
            // a forty line block that can only ever fire on that exact
            // sequence; a run is only worth describing as a whole when line
            // rules failed on most of it.
            let uncovered = cov.iter().filter(|c| !**c).count();
            if uncovered * 2 <= run.len() {
                continue;
            }
            if run.len() > MAX_BLOCK_LINES {
                rejected.insert(run[0].clone(), Rejection::BlockTooTall { lines: run.len() });
                continue;
            }
            let Some(templates) = run
                .iter()
                .map(|l| Template::from_line(l))
                .collect::<Option<Vec<Template>>>()
            else {
                continue;
            };
            let Some(pattern) = Pattern::block(templates) else {
                continue;
            };
            match crate::rule::check(&pattern, &run, keep, &lesson.guards, lesson.force) {
                Ok(()) => {
                    let before = self.rules.len();
                    self.install(lesson, pattern, 1, &run[0], outcome);
                    if self.rules.len() == before && outcome.reinforced.is_empty() {
                        continue;
                    }
                    made += 1;
                    // The lines are handled now, so their individual
                    // rejections are no longer the truth about them.
                    for line in &run {
                        rejected.remove(line);
                    }
                }
                Err(reason) => {
                    // A block level refusal says more than the per line one it
                    // replaces: it is the reason the run as a whole was refused.
                    rejected.insert(run[0].clone(), reason);
                }
            }
        }
        made
    }

    /// Turn `<filter-fold as="…">` runs into rules that leave one line behind.
    ///
    /// A fold is learned as a *line* rule wherever the run has a repeating
    /// shape, because that is what makes it useful: forty deprecation warnings
    /// collapse to one summary whether the next run prints forty of them or
    /// three. Only a run with no common shape — a banner — falls back to a
    /// block fold, which needs the exact run to match again.
    fn learn_folds(
        &mut self,
        lesson: &Lesson<'_>,
        keep: &[String],
        rejected: &mut BTreeMap<String, Rejection>,
        outcome: &mut LearnOutcome,
    ) -> u64 {
        let mut made = 0;
        for group in &lesson.annotated.folds {
            if group.lines.is_empty() {
                continue;
            }
            if group.summary.trim().is_empty() {
                // No caption is no fold. Say so rather than inventing one: a
                // summary a machine wrote is exactly the abstractive step this
                // project refuses to take.
                rejected.insert(group.lines[0].clone(), Rejection::FoldWithoutSummary);
                continue;
            }
            let action = Action::Fold {
                summary: group.summary.trim().to_string(),
            };

            // One candidate per distinct shape in the run.
            let mut shapes: BTreeMap<String, (Template, String, u32)> = BTreeMap::new();
            for line in &group.lines {
                if let Some(t) = Template::from_line(line) {
                    let entry = shapes
                        .entry(t.canonical())
                        .or_insert_with(|| (t, line.clone(), 0));
                    entry.2 += 1;
                }
            }
            let mut candidates: Vec<(Template, String, u32)> = shapes.into_values().collect();
            self.generalise(&mut candidates, keep, lesson);

            let mut installed = 0;
            for (template, source, examples) in &candidates {
                let pattern = Pattern::line(template.clone());
                if crate::rule::check(
                    &pattern,
                    std::slice::from_ref(source),
                    keep,
                    &lesson.guards,
                    lesson.force,
                )
                .is_err()
                {
                    continue;
                }
                self.install_action(lesson, pattern, action.clone(), *examples, source, outcome);
                installed += 1;
                made += 1;
                rejected.remove(source);
            }

            // Nothing had a usable shape on its own: try the run as a block.
            if installed == 0 && lesson.guards.learn_blocks && group.lines.len() >= 2 {
                let Some(templates) = group
                    .lines
                    .iter()
                    .map(|l| Template::from_line(l))
                    .collect::<Option<Vec<Template>>>()
                else {
                    continue;
                };
                let Some(pattern) = Pattern::block(templates) else {
                    continue;
                };
                match crate::rule::check(&pattern, &group.lines, keep, &lesson.guards, lesson.force)
                {
                    Ok(()) => {
                        self.install_action(
                            lesson,
                            pattern,
                            action.clone(),
                            1,
                            &group.lines[0],
                            outcome,
                        );
                        made += 1;
                        for line in &group.lines {
                            rejected.remove(line);
                        }
                    }
                    Err(reason) => {
                        rejected.insert(group.lines[0].clone(), reason);
                    }
                }
            }
        }
        made
    }

    /// Turn `<filter-only>` lines into whitelist rules.
    ///
    /// These are checked against an *empty* keep set on purpose. A keep rule
    /// never removes the text it matches, so "would this also match something
    /// you kept?" is not a danger here — it is the entire point. What it still
    /// has to clear is the template-quality bar: a whitelist rule with no
    /// literal content keeps everything, which is a rule that does nothing.
    fn learn_keeps(
        &mut self,
        lesson: &Lesson<'_>,
        rejected: &mut BTreeMap<String, Rejection>,
        outcome: &mut LearnOutcome,
    ) -> u64 {
        let mut made = 0;
        let mut seen: BTreeMap<String, (Template, String)> = BTreeMap::new();
        for line in &lesson.annotated.only {
            if let Some(t) = Template::from_line(line) {
                seen.entry(t.canonical())
                    .or_insert_with(|| (t, line.clone()));
            }
        }
        for (template, source) in seen.into_values() {
            let pattern = Pattern::line(template);
            if let Err(reason) = crate::rule::check(
                &pattern,
                std::slice::from_ref(&source),
                &[],
                &lesson.guards,
                // A whitelist rule about an error line is not dangerous: it
                // keeps that line. The error guard exists to stop removals.
                true,
            ) {
                rejected.insert(source, reason);
                continue;
            }
            self.install_action(lesson, pattern, Action::Keep, 1, &source, outcome);
            made += 1;
        }
        made
    }

    /// Greedily merge pairs of templates. Returns how many shapes were folded.
    fn generalise(
        &self,
        candidates: &mut Vec<(Template, String, u32)>,
        keep: &[String],
        lesson: &Lesson<'_>,
    ) -> u64 {
        let mut folded = 0u64;
        let mut changed = true;
        // Bounded: each pass strictly shrinks `candidates`, and the loop stops
        // as soon as a pass finds nothing.
        while changed {
            changed = false;
            'outer: for i in 0..candidates.len() {
                for j in (i + 1)..candidates.len() {
                    let Some(merged) = candidates[i].0.merge(&candidates[j].0) else {
                        continue;
                    };
                    // A widened pattern has to clear the guards on its own; a
                    // merge is never a way around them.
                    if crate::rule::check(
                        &Pattern::line(merged.clone()),
                        &[candidates[i].1.clone()],
                        keep,
                        &lesson.guards,
                        lesson.force,
                    )
                    .is_err()
                    {
                        continue;
                    }
                    let (_, _, n) = candidates.remove(j);
                    candidates[i].0 = merged;
                    candidates[i].2 = candidates[i].2.saturating_add(n);
                    folded += 1;
                    changed = true;
                    break 'outer;
                }
            }
        }
        folded
    }

    // -- persistence -------------------------------------------------------

    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::storage(format!("cannot read {}: {e}", path.display())))?;
        Self::from_json(&text)
            .map_err(|e| Error::storage(format!("invalid rule file {}: {e}", path.display())))
    }

    pub fn from_json(text: &str) -> Result<Self> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let set: RuleSet = serde_json::from_str(text).map_err(Error::other)?;
        if set.schema_version > RULES_SCHEMA_VERSION {
            return Err(Error::other(format!(
                "rule file schema v{} is newer than this ttk understands (v{RULES_SCHEMA_VERSION})",
                set.schema_version
            )));
        }
        Ok(set)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a rule set is serializable")
    }

    /// Write atomically: a crash mid-write must not cost the whole file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut text = self.to_json();
        text.push('\n');
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// `<workspace>/learned-filters.json`.
pub fn project_rules_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(RULES_FILE)
}

/// `<global home>/filters/learned-filters.json`: where `ttk learn --user`
/// writes, if the platform has a configuration directory.
///
/// `TTK_USER_RULES` overrides it completely. That override exists for the same
/// reason `TTK_HOME` does: a test run, or a sandboxed agent, must never be able
/// to reach the developer's real rule file.
pub fn user_rules_path() -> Option<PathBuf> {
    if let Some(p) = user_rules_override() {
        return Some(p);
    }
    let dir = ttk_core::config::global_filters_dir()?;
    migrate_legacy_user_rules(&dir);
    Some(dir.join(RULES_FILE))
}

/// Every global rule file, in load order.
///
/// The global `filters` folder is meant to be browsed and edited by hand, so
/// any `*.json` rule file dropped into it applies to every project, not only
/// the one `ttk learn --user` writes. `learned-filters.json` loads first; the
/// rest follow by name so the order never depends on the file system.
pub fn user_rule_files() -> Vec<PathBuf> {
    if let Some(p) = user_rules_override() {
        return vec![p];
    }
    let Some(main) = user_rules_path() else {
        return Vec::new();
    };
    let mut files = vec![main.clone()];
    if let Some(dir) = main.parent()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        let mut extra: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p != &main
                    && p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect();
        extra.sort();
        files.extend(extra);
    }
    files
}

fn user_rules_override() -> Option<PathBuf> {
    match std::env::var("TTK_USER_RULES") {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => None,
    }
}

/// Earlier versions kept the user rule file directly in the global home. Move
/// it into `filters/` once, so there is exactly one place to look. Best
/// effort: if the move fails the old file is simply not loaded, never lost.
fn migrate_legacy_user_rules(filters_dir: &Path) {
    let Some(home) = filters_dir.parent() else {
        return;
    };
    let legacy = home.join(RULES_FILE);
    let target = filters_dir.join(RULES_FILE);
    if legacy.is_file() && !target.exists() && std::fs::create_dir_all(filters_dir).is_ok() {
        let _ = std::fs::rename(&legacy, &target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate;

    fn lesson_from(text: &str, scope: Scope) -> (Annotated, Scope) {
        (annotate::parse(text), scope)
    }

    #[test]
    fn a_lesson_becomes_a_rule_that_matches_later_output() {
        let (a, scope) = lesson_from(
            "added 412 packages in 9s\n\
             <filter-trash>\n\
             npm WARN deprecated glob@7.2.3: Glob versions prior to v9 are unsupported\n\
             </filter-trash>\n",
            Scope::command("npm", "install"),
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, scope));
        assert_eq!(out.created.len(), 1);
        assert!(out.rejected.is_empty(), "{:?}", out.rejected);
        assert_eq!(set.len(), 1);

        let rule = &set.rules[0];
        assert!(rule.matches(
            "npm WARN deprecated inflight@1.0.6: Glob versions prior to v9 are unsupported"
        ));
        assert!(!rule.matches("added 412 packages in 9s"));
    }

    #[test]
    fn repeated_shapes_collapse_into_one_rule() {
        let mut text = String::from("<filter-trash>\n");
        for v in ["1.0.6", "7.2.3", "2.1.0"] {
            text.push_str(&format!(
                "npm WARN deprecated pkg@{v}: This module is not supported\n"
            ));
        }
        text.push_str("</filter-trash>\n");
        let a = annotate::parse(&text);
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert_eq!(set.len(), 1, "one shape, one rule");
        assert_eq!(out.created[0].examples, 3);
    }

    #[test]
    fn one_varying_word_widens_the_pattern() {
        let a = annotate::parse(
            "<filter-trash>\n\
             Downloading package alpha from the registry cache\n\
             Downloading package beta from the registry cache\n\
             </filter-trash>\n",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert_eq!(set.len(), 1);
        assert_eq!(out.generalised, 1);
        assert!(set.rules[0].matches("Downloading package gamma from the registry cache"));
    }

    #[test]
    fn widening_stops_at_a_line_you_kept() {
        let a = annotate::parse(
            "Downloading package critical from the registry cache\n\
             <filter-trash>\n\
             Downloading package alpha from the registry cache\n\
             Downloading package beta from the registry cache\n\
             </filter-trash>\n",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert_eq!(out.generalised, 0, "the merge would have eaten a kept line");
        assert_eq!(set.len(), 2, "so both shapes stay as their own rule");
        assert!(
            !set.rules
                .iter()
                .any(|r| r.matches("Downloading package critical from the registry cache"))
        );
    }

    #[test]
    fn errors_are_refused_and_reported() {
        let a = annotate::parse(
            "<filter-trash>\nFAILED tests/a.py::test_x - assert 200 == 401\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("pytest")));
        assert!(set.is_empty());
        assert_eq!(out.rejected.len(), 1);
        assert_eq!(out.rejected[0].reason, Rejection::LooksLikeAnError);
    }

    #[test]
    fn teaching_the_same_lesson_twice_reinforces_rather_than_duplicates() {
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 is not supported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        let scope = Scope::program("npm");
        set.learn(&Lesson::new(&a, scope.clone()));
        let second = set.learn(&Lesson::new(&a, scope));
        assert_eq!(set.len(), 1);
        assert!(second.created.is_empty());
        assert_eq!(second.already_covered, 1);
    }

    #[test]
    fn a_kept_line_retires_the_rule_that_would_remove_it() {
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 is not supported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        let retired =
            set.retire_matching(&["npm WARN deprecated other@2.0.0 is not supported".to_string()]);
        assert_eq!(retired.len(), 1);
        assert!(!set.rules[0].enabled);
        assert!(set.applicable(Some("npm"), None).is_empty());
    }

    #[test]
    fn scope_decides_whether_a_rule_is_applicable() {
        let a = annotate::parse("<filter-trash>\ncargo noise line here always\n</filter-trash>");
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::command("cargo", "build")));
        assert_eq!(set.applicable(Some("cargo"), Some("build")).len(), 1);
        assert!(set.applicable(Some("cargo"), Some("test")).is_empty());
        assert!(set.applicable(None, None).is_empty());
    }

    // -- block rules -------------------------------------------------------

    const BANNER: &str = "\
<filter-trash>
==========================================
   Acme build system, all rights reserved
==========================================
</filter-trash>
real output starts here
";

    #[test]
    fn a_run_no_line_rule_can_describe_becomes_a_block() {
        let a = annotate::parse(BANNER);
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("make")));

        assert_eq!(out.blocks, 1);
        assert!(
            out.rejected.is_empty(),
            "the block rescued the lines a line rule refused: {:?}",
            out.rejected
        );
        let block = set.rules.iter().find(|r| r.is_block()).expect("a block");
        assert_eq!(block.height(), 3);
        assert!(
            block.pattern.contains('\n'),
            "a block reads as several lines"
        );
    }

    /// The majority test: repeated noise that one line rule already handles
    /// must not *also* become a block that can only fire on that exact run.
    #[test]
    fn a_run_a_line_rule_already_covers_does_not_become_a_block() {
        let mut text = String::from("<filter-trash>\n");
        for v in ["1.0.6", "7.2.3", "2.7.1", "5.1.5"] {
            text.push_str(&format!(
                "npm WARN deprecated pkg@{v}: This module is not supported\n"
            ));
        }
        text.push_str("</filter-trash>\n");
        let a = annotate::parse(&text);
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert_eq!(out.blocks, 0);
        assert_eq!(set.len(), 1);
        assert!(!set.rules[0].is_block());
    }

    #[test]
    fn two_separate_runs_are_never_one_block() {
        let a = annotate::parse(
            "<filter-trash>\n=====\n</filter-trash>\n\
             something kept in between\n\
             <filter-trash>\n=====\n</filter-trash>\n",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::global()));
        assert_eq!(out.blocks, 0, "neither run is two lines long");
        assert!(set.is_empty());
        assert!(!out.rejected.is_empty(), "and the user is told why");
    }

    #[test]
    fn an_error_inside_the_run_refuses_the_block() {
        let a = annotate::parse(
            "<filter-trash>\n\
             ==========================================\n\
             FAILED tests/a.py::test_x - assert 200 == 401\n\
             ==========================================\n\
             </filter-trash>\n",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::global()));
        assert_eq!(out.blocks, 0);
        assert!(set.is_empty());
        assert!(
            out.rejected
                .iter()
                .any(|r| r.reason == Rejection::LooksLikeAnError),
            "{:?}",
            out.rejected
        );
    }

    #[test]
    fn a_block_that_would_eat_kept_text_is_refused_and_reported() {
        // The same banner appears unmarked further down, so removing it is
        // exactly what the lesson says not to do.
        let a = annotate::parse(
            "<filter-trash>\n\
             ==========================================\n\
             deploying to the staging environment now\n\
             ==========================================\n\
             </filter-trash>\n\
             ==========================================\n\
             deploying to the staging environment now\n\
             ==========================================\n",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::global()));
        assert_eq!(out.blocks, 0);
        assert!(
            out.rejected
                .iter()
                .any(|r| matches!(r.reason, Rejection::MatchesKeptLine { .. })),
            "{:?}",
            out.rejected
        );
    }

    #[test]
    fn switching_block_learning_off_says_so() {
        let a = annotate::parse(BANNER);
        let mut set = RuleSet::default();
        let mut lesson = Lesson::new(&a, Scope::global());
        lesson.guards.learn_blocks = false;
        let out = set.learn(&lesson);
        assert_eq!(out.blocks, 0);
        assert!(
            out.rejected
                .iter()
                .any(|r| r.reason == Rejection::BlocksDisabled),
            "{:?}",
            out.rejected
        );
    }

    #[test]
    fn a_block_survives_the_rule_file() {
        let a = annotate::parse(BANNER);
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::global()));
        let back = RuleSet::from_json(&set.to_json()).expect("round trip");
        let block = back.rules.iter().find(|r| r.is_block()).expect("a block");
        assert_eq!(block.height(), 3);
        let lines = vec![
            "==========================================",
            "   Acme build system, all rights reserved",
            "==========================================",
        ];
        assert_eq!(block.match_run(&lines, 0), Some(3));
    }

    #[test]
    fn teaching_the_same_banner_twice_reinforces_the_block() {
        let a = annotate::parse(BANNER);
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::global()));
        let before = set.len();
        let second = set.learn(&Lesson::new(&a, Scope::global()));
        assert_eq!(set.len(), before, "no duplicate rule");
        assert_eq!(second.blocks, 0);
        assert!(second.already_covered >= 3);
    }

    #[test]
    fn a_kept_line_retires_the_block_that_contained_it() {
        let a = annotate::parse(BANNER);
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::global()));
        let retired =
            set.retire_matching(&["==========================================".to_string()]);
        assert!(!retired.is_empty());
        assert!(
            set.rules
                .iter()
                .filter(|r| r.is_block())
                .all(|r| !r.enabled)
        );
    }

    #[test]
    fn round_trips_through_json() {
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 unsupported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        let back = RuleSet::from_json(&set.to_json()).expect("round trip");
        assert_eq!(back.len(), 1);
        assert_eq!(back.rules[0].id, set.rules[0].id);
        assert!(back.rules[0].matches("npm WARN deprecated x@9.9.9 unsupported"));
    }

    #[test]
    fn saving_and_loading_uses_the_real_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = project_rules_path(dir.path());
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 unsupported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        set.save(&path).expect("save");
        assert_eq!(RuleSet::load(&path).expect("load").len(), 1);
        // A missing file is an empty set, not an error.
        assert!(
            RuleSet::load(&dir.path().join("nope.json"))
                .expect("load")
                .is_empty()
        );
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_misread() {
        let err =
            RuleSet::from_json(r#"{"schema_version": 999, "rules": []}"#).expect_err("must refuse");
        assert!(err.to_string().contains("newer"), "{err}");
    }

    #[test]
    fn merging_two_sets_deduplicates_by_id() {
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 unsupported\n</filter-trash>",
        );
        let mut one = RuleSet::default();
        one.learn(&Lesson::new(&a, Scope::program("npm")));
        let mut two = RuleSet::default();
        two.learn(&Lesson::new(&a, Scope::program("npm")));
        two.rules[0].hits = 5;
        assert_eq!(one.merge(two), 0, "same lesson, same id");
        assert_eq!(one.len(), 1);
        assert_eq!(one.rules[0].hits, 5, "accounting is carried over");
    }

    #[test]
    fn pruning_drops_only_rules_that_never_fired() {
        let a = annotate::parse(
            "<filter-trash>\n\
             npm WARN deprecated pkg@1.0.6 unsupported\n\
             yarn info fetching package metadata now\n\
             </filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert_eq!(set.len(), 2);
        set.rules[0].hits = 1;
        let now = ttk_core::ids::now_millis();
        let dropped = set.prune(30, now + 31 * 86_400_000);
        assert_eq!(dropped.len(), 1);
        assert_eq!(set.len(), 1);
        assert!(set.rules[0].hits > 0);
    }

    #[test]
    fn resolving_accepts_a_unique_prefix() {
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 unsupported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        let id = set.rules[0].id.clone();
        assert_eq!(set.resolve(&id[..8]).expect("resolve").id, id);
        assert!(set.resolve("flt_zzzzzz").is_err());
    }
}
