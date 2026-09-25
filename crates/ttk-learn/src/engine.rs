//! Applying learned rules to text.
//!
//! The engine is deliberately dumb: one left to right pass, the most specific
//! rule wins, nothing is retried. It has no state and no way to loop. All the
//! intelligence lives in what was allowed to become a rule in the first place
//! ([`crate::rule::check`]), and everything it proposes is still subject to the
//! quality firewall in `ttk-core`.
//!
//! Block rules are tried before line rules, tallest first. That ordering is the
//! only heuristic in the module and it follows from what a block *is*: a block
//! was learned precisely because its lines are individually too generic to be
//! rules, so letting a line rule claim one of them first would be letting the
//! weaker evidence win.

use std::collections::BTreeMap;

use ttk_core::tokens::{self, TokenCount};

use crate::rule::{Action, Rule, looks_critical};
use crate::ruleset::RuleSet;

/// One rule's contribution to a single filter pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleHit {
    pub id: String,
    /// Readable pattern, so a report never has to look the rule up again.
    pub pattern: String,
    pub lines: u64,
    pub tokens: u64,
    /// How many times the rule fired, as opposed to how many lines it removed.
    /// They differ for a block rule.
    pub runs: u64,
}

/// The result of one filter pass.
#[derive(Debug, Clone)]
pub struct Filtered {
    pub text: String,
    pub removed_lines: u64,
    /// Lines a rule matched but that were kept anyway because they look like
    /// an error. A number above zero here means a rule is drifting.
    pub protected_lines: u64,
    /// Blank lines dropped because the removal of their neighbours left them
    /// stranded, or because they sat inside a matched block.
    pub blank_lines_collapsed: u64,
    /// Runs a fold rule replaced with a one line summary.
    pub folded_runs: u64,
    /// Lines removed because a `<filter-only>` whitelist did not name them.
    ///
    /// Counted separately because no individual rule removed them: they went
    /// because nothing kept them, which is a collective decision and has to be
    /// reported as one.
    pub whitelisted_away: u64,
    /// Sorted by tokens removed, descending.
    pub by_rule: Vec<RuleHit>,
    pub tokens_before: TokenCount,
    pub tokens_after: TokenCount,
}

impl Filtered {
    pub fn changed(&self) -> bool {
        self.removed_lines > 0 || self.blank_lines_collapsed > 0
    }

    pub fn tokens_saved(&self) -> TokenCount {
        self.tokens_before.saved_against(self.tokens_after)
    }

    /// `learned filter: -118 line(s) via 4 rule(s), -900 tokens`
    pub fn summary(&self) -> String {
        format!(
            "learned filter: -{} line(s) via {} rule(s), -{} tokens",
            self.removed_lines,
            self.by_rule.len(),
            self.tokens_saved()
        )
    }
}

/// Which rule, if any, removes each line.
///
/// `Some(i)` indexes into the rule slice that was passed in. This is the one
/// place where matching happens; both [`Engine::filter`] and the learner ask
/// the same question through it, so what the filter would remove and what the
/// learner considers already covered can never drift apart.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    pub by_line: Vec<Option<usize>>,
    /// Lines a rule matched but that the error guard kept.
    pub protected: u64,
}

/// Order rules so the most specific one is tried first: blocks before lines,
/// tallest block first, and a stable tiebreak on the id so a pass is
/// reproducible whatever order the rule file happens to be in.
fn ordered<'a>(rules: &[&'a Rule]) -> Vec<&'a Rule> {
    let mut out = rules.to_vec();
    out.sort_by(|a, b| b.height().cmp(&a.height()).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Walk `lines` once and decide what each rule removes.
///
/// `rules` must already be in the order [`ordered`] produces; both callers get
/// that by construction.
fn cover(rules: &[&Rule], lines: &[&str], protect_errors: bool) -> Coverage {
    let mut by_line: Vec<Option<usize>> = vec![None; lines.len()];
    let mut protected = 0u64;
    let mut i = 0usize;

    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let mut matched = false;
        for (r, rule) in rules.iter().enumerate() {
            let Some(consumed) = rule.match_run(lines, i) else {
                continue;
            };
            // The error guard outranks every rule, always. A learned filter may
            // not delete the one line that explains a failure, however
            // confidently it was taught — and for a block, one protected line
            // anywhere in the run protects the whole run.
            if protect_errors && lines[i..i + consumed].iter().any(|l| looks_critical(l)) {
                protected += 1;
                continue;
            }
            for slot in &mut by_line[i..i + consumed] {
                *slot = Some(r);
            }
            i += consumed;
            matched = true;
            break;
        }
        if !matched {
            i += 1;
        }
    }

    Coverage { by_line, protected }
}

/// The rules that apply to one piece of content, ready to run.
pub struct Engine<'a> {
    /// Drop and fold rules, most specific first.
    rules: Vec<&'a Rule>,
    /// `<filter-only>` rules. Kept apart because they do not match text to
    /// remove it — they match text to *save* it, and everything they do not
    /// save goes.
    keep_rules: Vec<&'a Rule>,
    protect_errors: bool,
}

impl<'a> Engine<'a> {
    /// Select the rules whose scope covers this command.
    ///
    /// `program`/`subcommand` are `None` for content that did not come from a
    /// command we ran (a `ttk compile` pipe). Only global rules apply then:
    /// a rule taught for `npm install` has no evidence that it is safe for
    /// text of unknown origin.
    pub fn for_command(set: &'a RuleSet, program: Option<&str>, subcommand: Option<&str>) -> Self {
        let applicable = set.applicable(program, subcommand);
        let (keep, active): (Vec<&Rule>, Vec<&Rule>) =
            applicable.into_iter().partition(|r| r.is_keep());
        Self {
            rules: ordered(&active),
            keep_rules: keep,
            protect_errors: true,
        }
    }

    /// Rules that name what to keep rather than what to remove.
    pub fn whitelist(&self) -> usize {
        self.keep_rules.len()
    }

    /// Turn off the error guard. Only `--force` reaches this, and it is
    /// recorded on the event when it does.
    pub fn protect_errors(mut self, protect: bool) -> Self {
        self.protect_errors = protect;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.keep_rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len() + self.keep_rules.len()
    }

    /// How many of the selected rules match runs rather than single lines.
    pub fn blocks(&self) -> usize {
        self.rules.iter().filter(|r| r.is_block()).count()
    }

    /// Remove every line a rule claims.
    pub fn filter(&self, text: &str) -> Filtered {
        let tokens_before = tokens::estimate(text);
        if self.is_empty() || text.is_empty() {
            return Filtered {
                text: text.to_string(),
                removed_lines: 0,
                protected_lines: 0,
                blank_lines_collapsed: 0,
                folded_runs: 0,
                whitelisted_away: 0,
                by_rule: Vec::new(),
                tokens_before,
                tokens_after: tokens_before,
            };
        }

        let lines: Vec<&str> = text.lines().collect();
        let coverage = cover(&self.rules, &lines, self.protect_errors);

        // Owned, because a fold rule contributes a line that is in no input.
        let mut kept: Vec<String> = Vec::new();
        // rule index → (lines, tokens, runs)
        let mut hits: BTreeMap<usize, (u64, u64, u64)> = BTreeMap::new();
        let mut keep_hits: BTreeMap<usize, u64> = BTreeMap::new();
        let mut removed = 0u64;
        let mut collapsed = 0u64;
        let mut folded_runs = 0u64;
        let mut whitelisted = 0u64;
        let mut previous_removed = false;
        let mut previous_rule: Option<usize> = None;
        // Lines the current fold run has swallowed, so its summary can say how
        // many there were.
        let mut fold_run: Option<(usize, u64)> = None;

        for (i, line) in lines.iter().enumerate() {
            match coverage.by_line[i] {
                Some(r) => {
                    let entry = hits.entry(r).or_insert((0, 0, 0));
                    if line.trim().is_empty() {
                        // A blank line inside a matched block goes with it, but
                        // it is bookkeeping, not a removed line of content.
                        collapsed += 1;
                    } else {
                        entry.0 += 1;
                        entry.1 += tokens::estimate(line).value;
                        removed += 1;
                    }
                    let new_run = previous_rule != Some(r) || !previous_removed;
                    if new_run {
                        entry.2 += 1;
                        flush_fold(&mut kept, &self.rules, &mut fold_run, &mut folded_runs);
                    }
                    if matches!(self.rules[r].action, Action::Fold { .. })
                        && !line.trim().is_empty()
                    {
                        let slot = fold_run.get_or_insert((r, 0));
                        slot.1 += 1;
                    }
                    previous_removed = true;
                    previous_rule = Some(r);
                }
                None => {
                    flush_fold(&mut kept, &self.rules, &mut fold_run, &mut folded_runs);
                    // A blank line left stranded by a removal goes with it.
                    if line.trim().is_empty() && previous_removed {
                        collapsed += 1;
                        continue;
                    }
                    previous_removed = false;
                    previous_rule = None;

                    // The whitelist, last: a line that survived every drop rule
                    // still has to be named by something, once any `<filter-only>`
                    // rule exists for this command.
                    if !self.keep_rules.is_empty() && !line.trim().is_empty() {
                        match self.keep_rules.iter().position(|k| k.matches(line)) {
                            Some(k) => {
                                *keep_hits.entry(k).or_insert(0) += 1;
                            }
                            None if self.protect_errors && looks_critical(line) => {
                                // The error guard outranks the whitelist too. A
                                // rule that says "only these lines matter" was
                                // written about a working run, and it does not
                                // get to decide that a failure does not matter.
                            }
                            None => {
                                whitelisted += 1;
                                removed += 1;
                                previous_removed = true;
                                continue;
                            }
                        }
                    }
                    kept.push((*line).to_string());
                }
            }
        }
        flush_fold(&mut kept, &self.rules, &mut fold_run, &mut folded_runs);

        let mut out = kept.join("\n");
        // Preserve the trailing newline of the input: a filter must not change
        // whether the last line is terminated.
        if text.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }

        let mut by_rule: Vec<RuleHit> = hits
            .into_iter()
            .filter(|(_, (lines, ..))| *lines > 0)
            .map(|(r, (lines, toks, runs))| RuleHit {
                id: self.rules[r].id.clone(),
                pattern: self.rules[r].pattern.clone(),
                lines,
                tokens: toks,
                runs,
            })
            .collect();
        // A keep rule's contribution is the lines it saved, not the ones it
        // removed, so it is reported with zero tokens and an honest run count
        // rather than being credited with the whitelist's collective saving.
        for (k, matched) in keep_hits {
            by_rule.push(RuleHit {
                id: self.keep_rules[k].id.clone(),
                pattern: self.keep_rules[k].pattern.clone(),
                lines: 0,
                tokens: 0,
                runs: matched,
            });
        }
        by_rule.sort_by_key(|h| (std::cmp::Reverse(h.tokens), h.id.clone()));

        let tokens_after = tokens::estimate(&out);
        Filtered {
            text: out,
            removed_lines: removed,
            protected_lines: coverage.protected,
            blank_lines_collapsed: collapsed,
            folded_runs,
            whitelisted_away: whitelisted,
            by_rule,
            tokens_before,
            tokens_after,
        }
    }
}

/// Emit the summary line for a fold run that has just ended.
fn flush_fold(
    kept: &mut Vec<String>,
    rules: &[&Rule],
    fold_run: &mut Option<(usize, u64)>,
    folded_runs: &mut u64,
) {
    let Some((r, lines)) = fold_run.take() else {
        return;
    };
    if lines == 0 {
        return;
    }
    if let Some(line) = rules[r].action.folded_line(lines) {
        kept.push(line);
        *folded_runs += 1;
    }
}

/// Which of `lines` the given rules already remove.
///
/// Used by the learner to answer "is this marked line something we already
/// handle?" with exactly the logic the filter uses, blocks included.
pub fn covered_lines(rules: &[&Rule], lines: &[&str], protect_errors: bool) -> Vec<bool> {
    let active: Vec<&Rule> = rules.iter().filter(|r| !r.is_keep()).copied().collect();
    let ordered = ordered(&active);
    cover(&ordered, lines, protect_errors)
        .by_line
        .into_iter()
        .map(|slot| slot.is_some())
        .collect()
}

/// Write a pass's accounting back into the rule set.
///
/// Kept out of [`Engine`] on purpose: filtering borrows the rules immutably and
/// happens on the hot path, while recording is a separate, explicit decision by
/// the caller — a dry run records nothing.
pub fn record(set: &mut RuleSet, filtered: &Filtered, now_millis: u64) {
    for hit in &filtered.by_rule {
        if let Some(rule) = set.get_mut(&hit.id) {
            rule.hits += hit.runs.max(1);
            rule.lines_removed += hit.lines;
            rule.tokens_saved += hit.tokens;
            rule.last_hit_millis = Some(now_millis);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate;
    use crate::rule::Scope;
    use crate::ruleset::Lesson;

    fn taught(trash: &[&str], scope: Scope) -> RuleSet {
        let mut text = String::from("<filter-trash>\n");
        for line in trash {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("</filter-trash>\n");
        let a = annotate::parse(&text);
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, scope));
        assert!(
            out.rejected.is_empty(),
            "fixture must be learnable: {out:?}"
        );
        set
    }

    const NOISE: &str = "npm WARN deprecated inflight@1.0.6: This module is not supported";

    /// A banner. The decoration lines are unlearnable on their own — one
    /// token each — so the run is described as a block. The title line in the
    /// middle happens to be perfectly learnable, and gets its own line rule as
    /// well; the block is what removes the banner, the line rule is what
    /// removes that title if it ever shows up alone.
    const BANNER: [&str; 3] = [
        "==========================================",
        "   Acme build system, all rights reserved",
        "==========================================",
    ];

    fn block_rule(set: &RuleSet) -> &Rule {
        set.rules
            .iter()
            .find(|r| r.is_block())
            .expect("the fixture must have produced a block rule")
    }

    #[test]
    fn a_learned_line_disappears_from_later_output() {
        let set = taught(&[NOISE], Scope::command("npm", "install"));
        let engine = Engine::for_command(&set, Some("npm"), Some("install"));
        let out = engine.filter(
            "npm WARN deprecated glob@7.2.3: This module is not supported\n\
             added 412 packages in 9s\n",
        );
        assert_eq!(out.text, "added 412 packages in 9s\n");
        assert_eq!(out.removed_lines, 1);
        assert_eq!(out.by_rule.len(), 1);
        assert_eq!(out.by_rule[0].runs, 1);
        assert!(out.by_rule[0].tokens > 0);
        assert!(out.tokens_saved().value > 0);
        assert!(out.changed());
    }

    #[test]
    fn a_rule_outside_its_scope_never_fires() {
        let set = taught(&[NOISE], Scope::command("npm", "install"));
        let engine = Engine::for_command(&set, Some("npm"), Some("test"));
        assert!(engine.is_empty());
        let text = format!("{NOISE}\n");
        assert_eq!(engine.filter(&text).text, text);
    }

    #[test]
    fn unattributed_content_only_sees_global_rules() {
        let scoped = taught(&[NOISE], Scope::command("npm", "install"));
        assert!(Engine::for_command(&scoped, None, None).is_empty());

        let global = taught(&[NOISE], Scope::global());
        assert_eq!(Engine::for_command(&global, None, None).len(), 1);
    }

    #[test]
    fn an_error_shaped_line_survives_even_a_matching_rule() {
        // Taught with --force, so the rule exists at all.
        let a = annotate::parse(
            "<filter-trash>\nnpm ERR! code ELIFECYCLE exit status 1\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        let mut lesson = Lesson::new(&a, Scope::program("npm"));
        lesson.force = true;
        set.learn(&lesson);
        assert_eq!(set.len(), 1);

        let line = "npm ERR! code ELIFECYCLE exit status 1\n";
        let engine = Engine::for_command(&set, Some("npm"), None);
        let guarded = engine.filter(line);
        assert_eq!(guarded.text, line, "the error guard outranks the rule");
        assert_eq!(guarded.protected_lines, 1);
        assert_eq!(guarded.removed_lines, 0);

        // …and can be switched off deliberately, which is what --force means.
        let unguarded = Engine::for_command(&set, Some("npm"), None).protect_errors(false);
        assert_eq!(unguarded.filter(line).removed_lines, 1);
    }

    #[test]
    fn blank_lines_stranded_by_a_removal_go_with_it() {
        let set = taught(&[NOISE], Scope::program("npm"));
        let engine = Engine::for_command(&set, Some("npm"), None);
        let out = engine.filter(&format!("head\n{NOISE}\n\ntail\n"));
        assert_eq!(out.text, "head\ntail\n");
        assert_eq!(out.blank_lines_collapsed, 1);
    }

    #[test]
    fn blank_lines_elsewhere_are_untouched() {
        let set = taught(&[NOISE], Scope::program("npm"));
        let engine = Engine::for_command(&set, Some("npm"), None);
        let text = "head\n\ntail\n";
        assert_eq!(engine.filter(text).text, text);
    }

    #[test]
    fn a_trailing_newline_is_preserved_either_way() {
        let set = taught(&[NOISE], Scope::program("npm"));
        let engine = Engine::for_command(&set, Some("npm"), None);
        assert_eq!(engine.filter("a\nb\n").text, "a\nb\n");
        assert_eq!(engine.filter("a\nb").text, "a\nb");
    }

    #[test]
    fn an_empty_rule_set_is_an_exact_pass_through() {
        let set = RuleSet::default();
        let engine = Engine::for_command(&set, Some("npm"), None);
        let text = "anything at all\n";
        let out = engine.filter(text);
        assert_eq!(out.text, text);
        assert!(!out.changed());
        assert_eq!(out.tokens_saved().value, 0);
    }

    #[test]
    fn recording_updates_the_accounting_of_the_rules_that_fired() {
        let mut set = taught(&[NOISE], Scope::program("npm"));
        let out = Engine::for_command(&set, Some("npm"), None).filter(&format!("{NOISE}\n"));
        record(&mut set, &out, 1_700_000_000_000);
        let rule = &set.rules[0];
        assert_eq!(rule.hits, 1);
        assert_eq!(rule.lines_removed, 1);
        assert!(rule.tokens_saved > 0);
        assert_eq!(rule.last_hit_millis, Some(1_700_000_000_000));
    }

    #[test]
    fn a_dry_run_records_nothing() {
        let mut set = taught(&[NOISE], Scope::program("npm"));
        Engine::for_command(&set, Some("npm"), None).filter(&format!("{NOISE}\n"));
        assert_eq!(set.rules[0].hits, 0);
        set.rules[0].enabled = false;
        assert!(Engine::for_command(&set, Some("npm"), None).is_empty());
    }

    #[test]
    fn the_summary_names_lines_rules_and_tokens() {
        let set = taught(&[NOISE], Scope::program("npm"));
        let out = Engine::for_command(&set, Some("npm"), None).filter(&format!("{NOISE}\n"));
        let s = out.summary();
        assert!(s.contains("1 line(s)"), "{s}");
        assert!(s.contains("1 rule(s)"), "{s}");
    }

    // -- block rules -------------------------------------------------------

    #[test]
    fn a_block_rule_removes_the_whole_banner() {
        let set = taught(&BANNER, Scope::program("make"));
        // Two rules: the block for the run, and a line rule for the one line
        // of it that carried enough signal on its own.
        assert_eq!(set.len(), 2, "{:#?}", set.rules);
        assert_eq!(set.rules.iter().filter(|r| r.is_block()).count(), 1);
        assert_eq!(block_rule(&set).height(), 3);

        let engine = Engine::for_command(&set, Some("make"), None);
        assert_eq!(engine.blocks(), 1);
        let out = engine.filter(&format!("{}\nreal output here\n", BANNER.join("\n")));
        assert_eq!(out.text, "real output here\n");
        assert_eq!(out.removed_lines, 3);
        assert_eq!(out.by_rule.len(), 1, "the block took the whole run alone");
        assert_eq!(out.by_rule[0].runs, 1, "one run, three lines");
        assert_eq!(out.by_rule[0].lines, 3);
    }

    /// Repeated noise must *not* turn into a block: one line rule already
    /// describes it, and a forty line block could only ever fire on that exact
    /// sequence again.
    #[test]
    fn a_run_that_one_line_rule_already_covers_stays_a_line_rule() {
        let mut lines = Vec::new();
        for v in ["1.0.6", "7.2.3", "2.7.1", "5.1.5"] {
            lines.push(format!(
                "npm WARN deprecated pkg@{v}: This module is not supported"
            ));
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let set = taught(&refs, Scope::program("npm"));
        assert_eq!(set.len(), 1);
        assert!(!set.rules[0].is_block(), "one shape, one line rule");
    }

    #[test]
    fn a_block_only_matches_the_whole_run() {
        let set = taught(&BANNER, Scope::program("make"));
        let block = block_rule(&set).clone();
        let only_block = RuleSet {
            rules: vec![block],
            ..RuleSet::default()
        };
        let engine = Engine::for_command(&only_block, Some("make"), None);
        // The middle line has gone, so the run is not there any more.
        let text = format!("{}\n{}\nreal output\n", BANNER[0], BANNER[2]);
        assert_eq!(
            engine.filter(&text).removed_lines,
            0,
            "two thirds of a banner is not the banner"
        );
    }

    #[test]
    fn a_blank_line_inside_a_banner_is_matched_and_collapsed() {
        let set = taught(&BANNER, Scope::program("make"));
        let engine = Engine::for_command(&set, Some("make"), None);
        let text = format!(
            "{}\n{}\n\n{}\nreal output\n",
            BANNER[0], BANNER[1], BANNER[2]
        );
        let out = engine.filter(&text);
        assert_eq!(out.text, "real output\n");
        assert_eq!(out.removed_lines, 3);
        assert_eq!(out.blank_lines_collapsed, 1);
    }

    /// Defence in depth: a block rule that somehow exists — forced, imported,
    /// or learned before a tool changed its wording — still may not delete a
    /// run that carries an error. The guard sits in the engine, not only in the
    /// learner, so it applies to rules the learner never approved.
    #[test]
    fn an_error_anywhere_in_the_run_saves_the_whole_run() {
        use crate::pattern::{Pattern, Template};

        let lines = [
            "==========================================",
            "   Acme toolchain summary: 3 builds failed",
            "==========================================",
        ];
        let templates: Vec<Template> = lines
            .iter()
            .map(|l| Template::from_line(l).expect("template"))
            .collect();
        let set = RuleSet {
            rules: vec![Rule::new(
                Scope::global(),
                Pattern::block(templates).expect("block"),
            )],
            ..RuleSet::default()
        };

        let text = format!("{}\ntail\n", lines.join("\n"));
        let guarded = Engine::for_command(&set, None, None).filter(&text);
        assert_eq!(guarded.removed_lines, 0, "{}", guarded.text);
        assert_eq!(guarded.protected_lines, 1);
        assert!(guarded.text.contains("3 builds failed"));

        // Switching the guard off is what --force means, and then it does fire.
        let unguarded = Engine::for_command(&set, None, None)
            .protect_errors(false)
            .filter(&text);
        assert_eq!(unguarded.removed_lines, 3);
    }

    #[test]
    fn a_banner_repeated_twice_is_two_runs_of_one_rule() {
        let set = taught(&BANNER, Scope::program("make"));
        let engine = Engine::for_command(&set, Some("make"), None);
        let _ = block_rule(&set);
        let banner = BANNER.join("\n");
        let out = engine.filter(&format!("{banner}\nmiddle\n{banner}\ntail\n"));
        assert_eq!(out.text, "middle\ntail\n");
        assert_eq!(out.removed_lines, 6);
        assert_eq!(out.by_rule[0].runs, 2);
        assert_eq!(out.by_rule[0].lines, 6);
    }

    /// A block was learned because its lines are individually too generic, so
    /// it has to be tried before any line rule that happens to match one of
    /// them — otherwise the weaker evidence would win and leave the decoration
    /// behind.
    #[test]
    fn blocks_are_tried_before_line_rules() {
        let set = taught(&BANNER, Scope::global());
        let line = set
            .rules
            .iter()
            .find(|r| !r.is_block())
            .expect("the title line is learnable on its own");
        assert!(line.matches(BANNER[1]));

        let out =
            Engine::for_command(&set, None, None).filter(&format!("{}\ntail\n", BANNER.join("\n")));
        assert_eq!(out.text, "tail\n");
        assert_eq!(out.removed_lines, 3);
        assert_eq!(out.by_rule.len(), 1, "the block took the whole run");
        assert_eq!(out.by_rule[0].id, block_rule(&set).id);

        // The line rule is not redundant: it still handles the title alone.
        let alone = Engine::for_command(&set, None, None)
            .filter("   Acme build system, all rights reserved\ntail\n");
        assert_eq!(alone.removed_lines, 1);
        assert_eq!(alone.by_rule[0].id, line.id);
    }

    // -- folds and whitelists ---------------------------------------------

    fn learn(text: &str, scope: Scope) -> RuleSet {
        let a = annotate::parse(text);
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, scope));
        assert!(
            out.rejected.is_empty(),
            "fixture must be learnable: {out:?}"
        );
        set
    }

    #[test]
    fn a_fold_replaces_a_whole_run_with_one_line() {
        let set = learn(
            "<filter-fold as=\"npm: deprecation warnings\">
             npm WARN deprecated inflight@1.0.6: This module is not supported
             npm WARN deprecated glob@7.2.3: This module is not supported
             </filter-fold>
             added 412 packages in 9s
",
            Scope::program("npm"),
        );
        assert_eq!(set.len(), 1, "one shape, one fold rule");
        assert!(matches!(
            set.rules[0].action,
            crate::rule::Action::Fold { .. }
        ));

        // Four warnings this time, and still one summary line.
        let text = concat!(
            "npm WARN deprecated a@1.0.0: This module is not supported
",
            "npm WARN deprecated b@2.0.0: This module is not supported
",
            "npm WARN deprecated c@3.0.0: This module is not supported
",
            "npm WARN deprecated d@4.0.0: This module is not supported
",
            "added 3 packages in 2s
",
        );
        let out = Engine::for_command(&set, Some("npm"), None).filter(text);
        assert_eq!(
            out.text,
            "[folded] npm: deprecation warnings (4 line(s))
added 3 packages in 2s
"
        );
        assert_eq!(out.removed_lines, 4);
        assert_eq!(out.folded_runs, 1);
        assert!(out.tokens_saved().value > 0);
    }

    #[test]
    fn two_separate_fold_runs_get_two_summaries() {
        let set = learn(
            "<filter-fold as=\"npm noise\">
             npm WARN deprecated a@1.0.0: This module is not supported
             </filter-fold>
             something real
",
            Scope::program("npm"),
        );
        let text = concat!(
            "npm WARN deprecated a@1.0.0: This module is not supported
",
            "something real
",
            "npm WARN deprecated b@2.0.0: This module is not supported
",
        );
        let out = Engine::for_command(&set, Some("npm"), None).filter(text);
        assert_eq!(out.folded_runs, 2, "{}", out.text);
        assert_eq!(out.text.matches("[folded] npm noise").count(), 2);
    }

    #[test]
    fn a_fold_without_a_caption_is_refused() {
        let a = annotate::parse(
            "<filter-fold>
npm WARN deprecated a@1.0.0: This module is not supported
</filter-fold>",
        );
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, Scope::program("npm")));
        assert!(set.is_empty());
        assert!(
            out.rejected
                .iter()
                .any(|r| r.reason == crate::rule::Rejection::FoldWithoutSummary),
            "{out:?}"
        );
    }

    #[test]
    fn a_whitelist_removes_everything_it_does_not_name() {
        let set = learn(
            "<filter-only>
Build succeeded in 4.1s
</filter-only>
",
            Scope::command("msbuild", "build"),
        );
        assert_eq!(set.len(), 1);
        assert!(set.rules[0].is_keep());

        let engine = Engine::for_command(&set, Some("msbuild"), Some("build"));
        assert_eq!(engine.whitelist(), 1);
        let out = engine.filter(concat!(
            "Restoring packages for alpha
",
            "Restoring packages for beta
",
            "Build succeeded in 9.7s
",
            "Copying files to output
",
        ));
        assert_eq!(
            out.text,
            "Build succeeded in 9.7s
"
        );
        assert_eq!(out.whitelisted_away, 3);
        assert_eq!(out.removed_lines, 3);
    }

    /// The whitelist is the one rule kind that removes text it never matched,
    /// so the error guard has to outrank it too.
    #[test]
    fn a_whitelist_never_removes_an_error() {
        let set = learn(
            "<filter-only>
Build succeeded in 4.1s
</filter-only>
",
            Scope::program("msbuild"),
        );
        let out = Engine::for_command(&set, Some("msbuild"), None).filter(concat!(
            "Restoring packages for alpha
",
            "error CS0103: the name 'foo' does not exist
",
            "Build succeeded in 9.7s
",
        ));
        assert!(out.text.contains("error CS0103"), "{}", out.text);
        assert!(!out.text.contains("Restoring"), "{}", out.text);
        assert_eq!(out.whitelisted_away, 1);
    }

    #[test]
    fn a_whitelist_only_applies_where_it_is_scoped() {
        let set = learn(
            "<filter-only>
Build succeeded in 4.1s
</filter-only>
",
            Scope::program("msbuild"),
        );
        let text = "some unrelated output
from another command
";
        assert_eq!(
            Engine::for_command(&set, Some("cargo"), None)
                .filter(text)
                .text,
            text
        );
    }

    #[test]
    fn coverage_answers_the_same_question_the_filter_does() {
        let set = taught(&BANNER, Scope::global());
        let rules: Vec<&Rule> = set.rules.iter().collect();
        assert!(rules.iter().any(|r| r.is_block()));
        let text = format!("{}\nreal output\n", BANNER.join("\n"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            covered_lines(&rules, &lines, true),
            vec![true, true, true, false]
        );
    }
}
