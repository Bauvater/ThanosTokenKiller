//! Checking new rules against output ttk has already stored.
//!
//! The learner judges a rule by the lesson it came from. That lesson is one
//! run; the stored originals are every run. Before a rule is installed it can
//! be replayed over them, which answers the questions a single lesson cannot:
//! does it fire at all, how much would it have saved, does it eat most of
//! some command's output, and — the one that matters — would it have matched
//! a line that explains a failure?
//!
//! Replay goes through [`Engine`] itself, so the numbers are what the filter
//! would really have done, blocks, folds, whitelists and error guard included.

use std::collections::BTreeSet;

use crate::audit::Sample;
use crate::engine::{Engine, solo_matches};
use crate::rule::{Rule, looks_critical};
use crate::ruleset::RuleSet;

/// Above this share of a sample's non-blank lines, a rule is suspicious.
pub const WARN_SHARE: f64 = 0.8;

/// What one rule would have done to the stored samples, on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleValidation {
    pub id: String,
    /// Samples the rule's scope and condition admit.
    pub samples: usize,
    /// Non-blank lines the rule's pattern matched, before the error guard.
    pub matched: u64,
    /// Lines it would have removed, with the error guard as requested.
    pub lines: u64,
    pub tokens: u64,
    /// Matched lines that the quality firewall considers critical.
    pub critical: u64,
    /// The largest share of one sample's non-blank lines it removed.
    pub max_share: f64,
}

/// The result of [`validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    /// Samples offered.
    pub samples: usize,
    /// What all the rules together would have removed.
    pub lines_removed: u64,
    pub tokens_saved: u64,
    /// Distinct critical lines any removing rule matched.
    pub critical_matches: u64,
    /// The largest share of one sample the rules together removed.
    pub max_share: f64,
    pub per_rule: Vec<RuleValidation>,
}

/// What to do with the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationVerdict {
    Ok,
    /// Install, but tell the user why it might be wrong.
    Warn(String),
    /// Do not install.
    Reject(String),
}

impl ValidationVerdict {
    pub fn is_reject(&self) -> bool {
        matches!(self, ValidationVerdict::Reject(_))
    }
}

impl Validation {
    /// Reject beats warn beats ok.
    ///
    /// * **Reject** when a rule matches a critical line anywhere in history.
    ///   The runtime guard would keep the line, but a rule that matches errors
    ///   was learned from the wrong evidence. This holds whatever
    ///   `protect_errors` was (it only changes the removal numbers); whether
    ///   `--force` overrides a rejection is the caller's decision.
    /// * **Warn** when the rules remove more than [`WARN_SHARE`] of some
    ///   sample, or when a rule matched nothing at all (it may still be right
    ///   for future runs — or there may be no history yet).
    pub fn verdict(&self) -> ValidationVerdict {
        let critical: Vec<&str> = self
            .per_rule
            .iter()
            .filter(|r| r.critical > 0)
            .map(|r| r.id.as_str())
            .collect();
        if !critical.is_empty() {
            return ValidationVerdict::Reject(format!(
                "{} critical line(s) in stored output match {}",
                self.critical_matches,
                critical.join(", ")
            ));
        }

        let mut warnings = Vec::new();
        if self.samples == 0 {
            warnings.push("no stored output to check against".to_string());
        } else {
            let idle: Vec<&str> = self
                .per_rule
                .iter()
                .filter(|r| r.matched == 0)
                .map(|r| r.id.as_str())
                .collect();
            if !idle.is_empty() {
                warnings.push(format!(
                    "{} matched nothing in {} stored output(s)",
                    idle.join(", "),
                    self.samples
                ));
            }
        }
        if self.max_share > WARN_SHARE {
            warnings.push(format!(
                "would remove {:.0}% of one stored output",
                self.max_share * 100.0
            ));
        }
        if warnings.is_empty() {
            ValidationVerdict::Ok
        } else {
            ValidationVerdict::Warn(warnings.join("; "))
        }
    }
}

/// Replay `rules` over `samples`.
///
/// Each rule is evaluated alone for its own line, and all of them together for
/// the totals, so two overlapping new rules are not counted twice. Disabled
/// rules are evaluated as if enabled: the question is what they *would* do.
pub fn validate(rules: &[&Rule], samples: &[Sample], protect_errors: bool) -> Validation {
    let per_rule: Vec<RuleValidation> = rules
        .iter()
        .map(|rule| validate_one(rule, samples, protect_errors))
        .collect();

    let together = RuleSet {
        rules: rules.iter().copied().map(enabled).collect(),
        ..RuleSet::default()
    };
    let mut lines_removed = 0;
    let mut tokens_saved = 0;
    let mut max_share: f64 = 0.0;
    let mut critical: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (s, sample) in samples.iter().enumerate() {
        if let Some(e) = effect_of(&together, sample, protect_errors) {
            lines_removed += e.lines;
            tokens_saved += e.tokens;
            max_share = max_share.max(e.share);
        }
        let lines: Vec<&str> = sample.text.lines().collect();
        for rule in rules.iter().filter(|r| !r.is_keep() && sample.admits(r)) {
            for (i, hit) in solo_matches(rule, &lines).into_iter().enumerate() {
                if hit && looks_critical(lines[i]) {
                    critical.insert((s, i));
                }
            }
        }
    }

    Validation {
        samples: samples.len(),
        lines_removed,
        tokens_saved,
        critical_matches: critical.len() as u64,
        max_share,
        per_rule,
    }
}

fn validate_one(rule: &Rule, samples: &[Sample], protect_errors: bool) -> RuleValidation {
    let alone = RuleSet {
        rules: vec![enabled(rule)],
        ..RuleSet::default()
    };
    let mut out = RuleValidation {
        id: rule.id.clone(),
        samples: 0,
        matched: 0,
        lines: 0,
        tokens: 0,
        critical: 0,
        max_share: 0.0,
    };
    for sample in samples {
        let Some(e) = effect_of(&alone, sample, protect_errors) else {
            continue;
        };
        out.samples += 1;
        out.lines += e.lines;
        out.tokens += e.tokens;
        out.max_share = out.max_share.max(e.share);
        let lines: Vec<&str> = sample.text.lines().collect();
        for (i, hit) in solo_matches(rule, &lines).into_iter().enumerate() {
            if !hit || lines[i].trim().is_empty() {
                continue;
            }
            out.matched += 1;
            // A whitelist rule keeps what it matches; matching an error is
            // exactly what it should do.
            if !rule.is_keep() && looks_critical(lines[i]) {
                out.critical += 1;
            }
        }
    }
    out
}

/// What a set of rules does to one sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Effect {
    pub lines: u64,
    pub tokens: u64,
    /// Removed lines over non-blank lines.
    pub share: f64,
}

/// Run `set` over `sample` through the real engine. `None` when no rule in
/// the set applies to the sample at all.
pub(crate) fn effect_of(set: &RuleSet, sample: &Sample, protect_errors: bool) -> Option<Effect> {
    let engine = Engine::for_context(
        set,
        sample.program.as_deref(),
        sample.subcommand.as_deref(),
        sample.exit_code,
    );
    if engine.is_empty() {
        return None;
    }
    let out = engine.protect_errors(protect_errors).filter(&sample.text);
    let non_blank = sample.text.lines().filter(|l| !l.trim().is_empty()).count();
    let share = if non_blank == 0 {
        0.0
    } else {
        out.removed_lines as f64 / non_blank as f64
    };
    Some(Effect {
        lines: out.removed_lines,
        tokens: out.tokens_saved().value,
        share,
    })
}

fn enabled(rule: &Rule) -> Rule {
    let mut r = rule.clone();
    r.enabled = true;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::annotate;
    use crate::pattern::{Pattern, Template};
    use crate::rule::Scope;
    use crate::ruleset::Lesson;

    const NOISE: &str = "npm WARN deprecated inflight@1.0.6: This module is not supported";

    fn learned(line: &str, scope: Scope) -> Rule {
        let a = annotate::parse(&format!("<filter-trash>\n{line}\n</filter-trash>\n"));
        let mut set = RuleSet::default();
        let out = set.learn(&Lesson::new(&a, scope));
        assert_eq!(out.created.len(), 1, "{out:?}");
        set.rules.remove(0)
    }

    fn npm(text: &str) -> Sample {
        Sample::new(Some("npm"), Some("install"), Some(0), text)
    }

    #[test]
    fn a_rule_that_fires_on_history_is_ok() {
        let rule = learned(NOISE, Scope::program("npm"));
        let samples = vec![
            npm("npm WARN deprecated glob@7.2.3: This module is not supported\nadded 3 packages\n"),
            npm("added 1 package\nup to date\n"),
        ];
        let v = validate(&[&rule], &samples, true);
        assert_eq!(v.samples, 2);
        assert_eq!(v.lines_removed, 1);
        assert!(v.tokens_saved > 0);
        assert_eq!(v.critical_matches, 0);
        assert_eq!(v.per_rule[0].samples, 2);
        assert_eq!(v.per_rule[0].matched, 1);
        assert!((v.max_share - 0.5).abs() < 1e-9);
        assert_eq!(v.verdict(), ValidationVerdict::Ok);
    }

    #[test]
    fn a_rule_that_matches_an_error_in_history_is_rejected() {
        // Built by hand: the learner would have refused it, but an imported
        // or forced rule can still look like this.
        let line = "npm ERR! code ELIFECYCLE exit status 1";
        let rule = Rule::new(
            Scope::program("npm"),
            Pattern::line(Template::from_line(line).expect("template")),
        );
        let samples = vec![npm(&format!("{line}\nadded 1 package\nmore output\n"))];
        let v = validate(&[&rule], &samples, true);
        assert_eq!(v.critical_matches, 1);
        assert_eq!(v.per_rule[0].critical, 1);
        assert_eq!(v.lines_removed, 0, "the guard still kept it");
        assert!(v.verdict().is_reject(), "{:?}", v.verdict());

        // Without the guard the line would really go, and it is still a
        // rejection: overriding it is the caller's call, not the verdict's.
        let unguarded = validate(&[&rule], &samples, false);
        assert_eq!(unguarded.lines_removed, 1);
        assert_eq!(unguarded.critical_matches, 1);
        assert!(unguarded.verdict().is_reject());
    }

    #[test]
    fn a_rule_that_never_matches_is_a_warning() {
        let rule = learned(NOISE, Scope::program("npm"));
        let samples = vec![npm("added 1 package\n")];
        let v = validate(&[&rule], &samples, true);
        assert_eq!(v.per_rule[0].matched, 0);
        assert!(matches!(v.verdict(), ValidationVerdict::Warn(m) if m.contains("matched nothing")));
    }

    #[test]
    fn no_history_is_a_warning_not_an_error() {
        let rule = learned(NOISE, Scope::program("npm"));
        let v = validate(&[&rule], &[], true);
        assert_eq!(v.samples, 0);
        assert!(
            matches!(v.verdict(), ValidationVerdict::Warn(m) if m.contains("no stored output"))
        );
    }

    #[test]
    fn a_rule_that_eats_most_of_a_sample_is_a_warning() {
        let rule = learned(NOISE, Scope::program("npm"));
        let text = format!("{NOISE}\n{NOISE}\n{NOISE}\n{NOISE}\nadded 1 package\n");
        let v = validate(&[&rule], &[npm(&text)], true);
        assert!((v.max_share - 0.8).abs() < 1e-9);
        assert_eq!(v.verdict(), ValidationVerdict::Ok, "exactly 80% is fine");

        let text = format!("{NOISE}\n{NOISE}\n{NOISE}\n{NOISE}\n{NOISE}\nadded 1 package\n");
        let v = validate(&[&rule], &[npm(&text)], true);
        assert!(v.max_share > WARN_SHARE);
        assert!(matches!(v.verdict(), ValidationVerdict::Warn(m) if m.contains('%')));
    }

    #[test]
    fn samples_outside_the_scope_or_condition_do_not_count() {
        let rule = learned(NOISE, Scope::command("npm", "install"));
        let samples = vec![
            Sample::new(Some("npm"), Some("test"), Some(0), NOISE),
            Sample::new(None, None, None, NOISE),
        ];
        let v = validate(&[&rule], &samples, true);
        assert_eq!(v.per_rule[0].samples, 0);
        assert_eq!(v.lines_removed, 0);

        let ok = rule.clone().with_when(crate::rule::When::Success);
        let failed = Sample::new(Some("npm"), Some("install"), Some(1), NOISE);
        assert_eq!(validate(&[&ok], &[failed], true).per_rule[0].samples, 0);
    }

    #[test]
    fn overlapping_rules_are_not_counted_twice_in_the_totals() {
        let a = learned(NOISE, Scope::program("npm"));
        let b = learned(NOISE, Scope::global());
        let text = format!("{NOISE}\nadded 1 package\n");
        let v = validate(&[&a, &b], &[npm(&text)], true);
        assert_eq!(v.per_rule[0].lines, 1);
        assert_eq!(v.per_rule[1].lines, 1);
        assert_eq!(v.lines_removed, 1, "one line, removed once");
    }

    #[test]
    fn a_whitelist_matching_an_error_is_not_critical() {
        let a = annotate::parse(
            "<filter-only>\nerror CS0103: the name foo does not exist\n</filter-only>\n",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("msbuild")));
        let rule = set.rules.remove(0);
        assert!(rule.is_keep());
        let samples = vec![Sample::new(
            Some("msbuild"),
            None,
            Some(1),
            "Restoring packages\nerror CS0103: the name foo does not exist\n",
        )];
        let v = validate(&[&rule], &samples, true);
        assert_eq!(v.critical_matches, 0);
        assert_eq!(v.per_rule[0].matched, 1);
        assert_eq!(v.lines_removed, 1, "the restore line went");
    }
}
