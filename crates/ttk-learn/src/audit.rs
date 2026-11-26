//! Finding rules that are redundant, dead, or doing more than they should.
//!
//! A rule file grows by accretion: every lesson adds, almost nothing removes.
//! After a few months it holds rules that duplicate each other, rules that a
//! later, wider rule made pointless, rules for a tool version nobody runs any
//! more, and — rarely, but it is the case that matters — a rule that removes
//! nearly everything a command prints.
//!
//! [`audit`] reports all of those. It never changes anything; the caller
//! decides. [`redundant_ids`] is the list meant for `ttk rules prune
//! --redundant`. Removing a *duplicate* changes no output by construction;
//! a *subsumed* rule is judged on the stored samples only, so it may still be
//! the only rule for a line the history never showed.
//!
//! The redundancy checks are careful never to flag both members of a pair.
//! Every rule has a rank — wider scope, then unconditioned, then older, then
//! the smaller id — and of two rules that cover each other, only the one that
//! ranks lower is reported.

use std::collections::BTreeSet;

use crate::engine::solo_matches;
use crate::rule::Rule;
use crate::ruleset::RuleSet;
use crate::validate::effect_of;

const DAY_MILLIS: u64 = 86_400_000;

/// One stored output a rule can be replayed against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sample {
    pub program: Option<String>,
    pub subcommand: Option<String>,
    /// `None` when unknown; only unconditioned rules apply then.
    pub exit_code: Option<i32>,
    pub text: String,
}

impl Sample {
    pub fn new(
        program: Option<&str>,
        subcommand: Option<&str>,
        exit_code: Option<i32>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            program: program.map(str::to_string),
            subcommand: subcommand.map(str::to_string),
            exit_code,
            text: text.into(),
        }
    }

    /// Would `rule` be selected for this sample? Scope and condition only;
    /// whether it is enabled is the caller's question.
    pub fn admits(&self, rule: &Rule) -> bool {
        rule.applies(
            self.program.as_deref(),
            self.subcommand.as_deref(),
            self.exit_code,
        )
    }
}

/// Thresholds for [`audit`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuditOptions {
    /// A rule that has never fired is reported once it is this many days old.
    pub never_hit_days: u64,
    /// A rule whose last hit is older than this is reported as stale.
    pub stale_days: u64,
    /// A rule that removes more than this share of its samples' non-blank
    /// lines, on average, is reported as overbroad.
    pub max_share: f64,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            never_hit_days: 14,
            stale_days: 60,
            max_share: 0.8,
        }
    }
}

/// What is wrong with a rule.
#[derive(Debug, Clone, PartialEq)]
pub enum FindingKind {
    /// Same pattern and action as `other`, which fires everywhere this one
    /// does.
    Duplicate { other: String },
    /// Across the samples, every line this rule matched was matched by `by`
    /// too, and `by` fires everywhere this one does.
    Subsumed { by: String },
    /// Has never fired, and is old enough that it should have.
    NeverHit,
    /// Has not fired for `days` days.
    Stale { days: u64 },
    /// Has saved fewer tokens than it cost to teach.
    NegativeNet { net: i64 },
    /// Switched off.
    Disabled,
    /// Removes `share` of the non-blank lines of its samples, on average.
    Overbroad { share: f64 },
}

impl FindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingKind::Duplicate { .. } => "duplicate",
            FindingKind::Subsumed { .. } => "subsumed",
            FindingKind::NeverHit => "never-hit",
            FindingKind::Stale { .. } => "stale",
            FindingKind::NegativeNet { .. } => "negative-net",
            FindingKind::Disabled => "disabled",
            FindingKind::Overbroad { .. } => "overbroad",
        }
    }

    /// Another rule does this rule's work: certainly for a duplicate, on the
    /// evidence of the samples for a subsumed rule.
    pub fn is_redundant(&self) -> bool {
        matches!(
            self,
            FindingKind::Duplicate { .. } | FindingKind::Subsumed { .. }
        )
    }
}

/// One thing [`audit`] found about one rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub rule: String,
    pub kind: FindingKind,
    /// A sentence for a human.
    pub detail: String,
}

impl Finding {
    /// `flt_0123456789 [duplicate] same pattern and action as flt_…`
    pub fn summary(&self) -> String {
        format!("{} [{}] {}", self.rule, self.kind.as_str(), self.detail)
    }
}

/// Inspect every rule of `set`, replaying them over `samples` where a check
/// needs evidence. Findings are ordered by rule id, then by kind.
pub fn audit(
    set: &RuleSet,
    samples: &[Sample],
    now_millis: u64,
    opts: &AuditOptions,
) -> Vec<Finding> {
    let mut rules: Vec<&Rule> = set.rules.iter().collect();
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    let live: Vec<&Rule> = rules.iter().copied().filter(|r| r.enabled).collect();

    // Every line rule's solo matches over the samples, computed once.
    let coverage: Vec<Option<BTreeSet<(usize, usize)>>> = live
        .iter()
        .map(|r| (!r.is_block() && !r.is_keep()).then(|| matched_lines(r, samples)))
        .collect();

    let mut findings = Vec::new();
    for rule in &rules {
        if !rule.enabled {
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::Disabled,
                detail: "disabled; `ttk rules enable` or forget it".to_string(),
            });
            continue;
        }
        let a = live
            .iter()
            .position(|r| r.id == rule.id)
            .expect("an enabled rule is live");

        let duplicate = duplicate_of(rule, &live);
        if let Some(other) = duplicate {
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::Duplicate {
                    other: other.id.clone(),
                },
                detail: format!(
                    "same pattern and action as {}, which applies to {}{}",
                    other.id,
                    other.scope,
                    when_suffix(other)
                ),
            });
        } else if let Some(by) = subsumed_by(a, &live, &coverage) {
            let lines = coverage[a].as_ref().map_or(0, BTreeSet::len);
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::Subsumed { by: by.id.clone() },
                detail: format!(
                    "all {lines} line(s) it matched in stored output are matched by {} too",
                    by.id
                ),
            });
        }

        let age = now_millis.saturating_sub(rule.created_millis);
        let old_enough = age > opts.never_hit_days.saturating_mul(DAY_MILLIS);
        if rule.hits == 0 && old_enough {
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::NeverHit,
                detail: format!("never fired in {} day(s)", age / DAY_MILLIS),
            });
        }
        if rule.hits > 0
            && let Some(last) = rule.last_hit_millis
        {
            let since = now_millis.saturating_sub(last);
            if since > opts.stale_days.saturating_mul(DAY_MILLIS) {
                let days = since / DAY_MILLIS;
                findings.push(Finding {
                    rule: rule.id.clone(),
                    kind: FindingKind::Stale { days },
                    detail: format!("last fired {days} day(s) ago"),
                });
            }
        }
        let net = rule.net_tokens();
        if (rule.hits > 0 || old_enough) && net < 0 {
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::NegativeNet { net },
                detail: format!(
                    "saved {} token(s) but cost {} to teach",
                    rule.tokens_saved, rule.teach_tokens
                ),
            });
        }
        // A whitelist is built to remove most of what it sees; that is its
        // job, not a sign of over-reach.
        if !rule.is_keep()
            && let Some(share) = average_share(rule, samples)
            && share > opts.max_share
        {
            findings.push(Finding {
                rule: rule.id.clone(),
                kind: FindingKind::Overbroad { share },
                detail: format!(
                    "removes {:.0}% of the non-blank lines of its stored output on average",
                    share * 100.0
                ),
            });
        }
    }
    findings
}

/// The ids worth removing without further thought: duplicates and subsumed
/// rules. Sorted, each once.
pub fn redundant_ids(findings: &[Finding]) -> Vec<String> {
    let ids: BTreeSet<&str> = findings
        .iter()
        .filter(|f| f.kind.is_redundant())
        .map(|f| f.rule.as_str())
        .collect();
    ids.into_iter().map(str::to_string).collect()
}

// ---------------------------------------------------------------------------

/// Total order on rules: the *smaller* key survives a redundancy pair. Wider
/// scope first, then unconditioned, then older, then the smaller id.
fn rank(r: &Rule) -> (u8, u8, u64, &str) {
    (
        r.scope.specificity(),
        u8::from(!r.when.is_any()),
        r.created_millis,
        r.id.as_str(),
    )
}

/// Does `b` fire in every context `a` fires in?
fn covers(b: &Rule, a: &Rule) -> bool {
    b.scope.contains(&a.scope) && b.when.contains(a.when)
}

/// The best-ranked other rule with the same pattern and action that fires
/// everywhere `rule` does, if it outranks `rule`.
fn duplicate_of<'a>(rule: &Rule, live: &[&'a Rule]) -> Option<&'a Rule> {
    let canonical = rule.template.canonical();
    live.iter()
        .copied()
        .filter(|b| {
            b.id != rule.id
                && b.action == rule.action
                && covers(b, rule)
                && rank(b) < rank(rule)
                && b.template.canonical() == canonical
        })
        .min_by(|x, y| rank(x).cmp(&rank(y)))
}

/// Another line rule that matched every line `live[a]` matched, fires
/// everywhere it does, and — when the two matched exactly the same lines —
/// outranks it.
fn subsumed_by<'a>(
    a: usize,
    live: &[&'a Rule],
    coverage: &[Option<BTreeSet<(usize, usize)>>],
) -> Option<&'a Rule> {
    let rule = live[a];
    let mine = coverage[a].as_ref()?;
    if mine.is_empty() {
        return None;
    }
    live.iter()
        .enumerate()
        .filter(|(b, other)| *b != a && other.action == rule.action && covers(other, rule))
        .filter_map(|(b, other)| {
            let theirs = coverage[b].as_ref()?;
            if !mine.is_subset(theirs) {
                return None;
            }
            // Equal coverage: only one of the two may go.
            if mine.len() == theirs.len() && rank(other) > rank(rule) {
                return None;
            }
            Some(*other)
        })
        .min_by(|x, y| rank(x).cmp(&rank(y)))
}

/// `(sample, line)` for every non-blank line `rule` matches on its own, over
/// the samples it applies to.
fn matched_lines(rule: &Rule, samples: &[Sample]) -> BTreeSet<(usize, usize)> {
    let mut out = BTreeSet::new();
    for (s, sample) in samples.iter().enumerate() {
        if !sample.admits(rule) {
            continue;
        }
        let lines: Vec<&str> = sample.text.lines().collect();
        for (i, hit) in solo_matches(rule, &lines).into_iter().enumerate() {
            if hit && !lines[i].trim().is_empty() {
                out.insert((s, i));
            }
        }
    }
    out
}

/// Mean share of non-blank lines removed, over the samples in scope, when
/// there are at least two of them.
fn average_share(rule: &Rule, samples: &[Sample]) -> Option<f64> {
    let alone = RuleSet {
        rules: vec![rule.clone()],
        ..RuleSet::default()
    };
    let shares: Vec<f64> = samples
        .iter()
        .filter_map(|s| effect_of(&alone, s, true))
        .map(|e| e.share)
        .collect();
    (shares.len() >= 2).then(|| shares.iter().sum::<f64>() / shares.len() as f64)
}

fn when_suffix(r: &Rule) -> String {
    if r.when.is_any() {
        String::new()
    } else {
        format!(" (on {})", r.when)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::annotate;
    use crate::rule::{Scope, When};
    use crate::ruleset::Lesson;

    const NOW: u64 = 1_800_000_000_000;
    const NOISE: &str = "npm WARN deprecated inflight@1.0.6: This module is not supported";

    /// A rule as if learned `days_ago`, with `hits` hits.
    fn rule(line: &str, scope: Scope, days_ago: u64) -> Rule {
        let a = annotate::parse(&format!("<filter-trash>\n{line}\n</filter-trash>\n"));
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, scope));
        let mut r = set.rules.remove(0);
        r.created_millis = NOW - days_ago * DAY_MILLIS;
        r.teach_tokens = 0;
        r
    }

    fn set_of(rules: Vec<Rule>) -> RuleSet {
        RuleSet {
            rules,
            ..RuleSet::default()
        }
    }

    fn kinds(findings: &[Finding], id: &str) -> Vec<&'static str> {
        findings
            .iter()
            .filter(|f| f.rule == id)
            .map(|f| f.kind.as_str())
            .collect()
    }

    fn npm(text: &str) -> Sample {
        Sample::new(Some("npm"), Some("install"), Some(0), text)
    }

    /// The noise line with one word widened, so it also matches
    /// `… This package is not supported`.
    const WIDER_TOO: &str = "npm WARN deprecated inflight@1.0.6: This package is not supported";

    fn wider(scope: Scope) -> Rule {
        use crate::pattern::{Pattern, Template};
        let t = Template::from_line(NOISE)
            .expect("t")
            .merge(&Template::from_line(WIDER_TOO).expect("t"))
            .expect("one differing word merges");
        let r = Rule::new(scope, Pattern::line(t));
        assert!(r.matches(NOISE) && r.matches(WIDER_TOO));
        r
    }

    #[test]
    fn a_healthy_rule_has_no_findings() {
        let mut r = rule(NOISE, Scope::program("npm"), 30);
        r.hits = 4;
        r.tokens_saved = 100;
        r.last_hit_millis = Some(NOW - DAY_MILLIS);
        let set = set_of(vec![r]);
        let samples = vec![npm(&format!("{NOISE}\nadded 1\nok\n"))];
        assert!(audit(&set, &samples, NOW, &AuditOptions::default()).is_empty());
    }

    #[test]
    fn a_narrower_copy_of_a_wider_rule_is_a_duplicate() {
        let wide = rule(NOISE, Scope::program("npm"), 30);
        let narrow = rule(NOISE, Scope::command("npm", "install"), 30);
        let (wide_id, narrow_id) = (wide.id.clone(), narrow.id.clone());
        let set = set_of(vec![narrow, wide]);
        let f = audit(&set, &[], NOW, &AuditOptions::default());
        let dup: Vec<&Finding> = f.iter().filter(|f| f.kind.is_redundant()).collect();
        assert_eq!(dup.len(), 1, "{f:#?}");
        assert_eq!(dup[0].rule, narrow_id);
        assert_eq!(
            dup[0].kind,
            FindingKind::Duplicate {
                other: wide_id.clone()
            }
        );
        assert_eq!(redundant_ids(&f), vec![narrow_id]);
        assert!(dup[0].summary().contains("[duplicate]"));
    }

    #[test]
    fn a_conditioned_copy_of_an_unconditioned_rule_is_a_duplicate() {
        let any = rule(NOISE, Scope::program("npm"), 30);
        let ok = any.clone().with_when(When::Success);
        let fail = any.clone().with_when(When::Failure);
        let set = set_of(vec![ok.clone(), fail.clone(), any.clone()]);
        let f = audit(&set, &[], NOW, &AuditOptions::default());
        assert_eq!(redundant_ids(&f), {
            let mut v = vec![ok.id.clone(), fail.id.clone()];
            v.sort();
            v
        });

        // Success and failure variants alone are disjoint, not duplicates.
        let set = set_of(vec![ok, fail]);
        assert!(redundant_ids(&audit(&set, &[], NOW, &AuditOptions::default())).is_empty());
    }

    #[test]
    fn rules_that_overlap_without_covering_each_other_are_both_kept() {
        // Global but success-only, against npm-only but always: each fires
        // somewhere the other does not.
        let global_ok = rule(NOISE, Scope::global(), 30).with_when(When::Success);
        let npm_any = rule(NOISE, Scope::program("npm"), 30);
        let set = set_of(vec![global_ok, npm_any]);
        assert!(redundant_ids(&audit(&set, &[], NOW, &AuditOptions::default())).is_empty());
    }

    #[test]
    fn a_different_action_is_not_a_duplicate() {
        let drop = rule(NOISE, Scope::program("npm"), 30);
        let mut fold = rule(NOISE, Scope::command("npm", "install"), 30);
        fold.action = crate::rule::Action::Fold {
            summary: "npm noise".into(),
        };
        let set = set_of(vec![drop, fold]);
        let samples = vec![npm(&format!("{NOISE}\nok\n"))];
        assert!(redundant_ids(&audit(&set, &samples, NOW, &AuditOptions::default())).is_empty());
    }

    #[test]
    fn a_rule_whose_lines_another_rule_already_matches_is_subsumed() {
        // Two different patterns. The narrow one only ever matches lines the
        // wide one matches too.
        let narrow = rule(
            "npm WARN deprecated inflight@1.0.6: This module is not supported",
            Scope::command("npm", "install"),
            30,
        );
        let wide = wider(Scope::program("npm"));
        assert_ne!(wide.template.canonical(), narrow.template.canonical());
        let (wide_id, narrow_id) = (wide.id.clone(), narrow.id.clone());
        let samples = vec![
            npm(&format!("{NOISE}\nadded 3\n")),
            npm(&format!("{WIDER_TOO}\n")),
        ];
        let set = set_of(vec![narrow, wide]);
        let f = audit(&set, &samples, NOW, &AuditOptions::default());
        assert_eq!(
            f.iter()
                .find(|f| f.rule == narrow_id)
                .map(|f| f.kind.clone()),
            Some(FindingKind::Subsumed {
                by: wide_id.clone()
            }),
            "{f:#?}"
        );
        assert!(kinds(&f, &wide_id).iter().all(|k| *k != "subsumed"));
        assert_eq!(redundant_ids(&f), vec![narrow_id]);
    }

    #[test]
    fn of_two_rules_with_identical_coverage_only_one_is_flagged() {
        // Same scope, different patterns, and the samples only ever show lines
        // both match: exactly one of them may go.
        let mut a = wider(Scope::program("npm"));
        let mut b = rule(NOISE, Scope::program("npm"), 10);
        a.created_millis = NOW - 30 * DAY_MILLIS;
        b.created_millis = NOW - 10 * DAY_MILLIS;
        assert_ne!(a.template.canonical(), b.template.canonical());
        let samples = vec![
            npm(&format!("{NOISE}\nok\n")),
            npm(&format!("x\n{NOISE}\n")),
        ];
        let set = set_of(vec![a.clone(), b.clone()]);
        let f = audit(&set, &samples, NOW, &AuditOptions::default());
        // The newer one goes; the older one survives.
        assert_eq!(redundant_ids(&f), vec![b.id.clone()], "{f:#?}");
    }

    #[test]
    fn a_rule_that_never_matched_a_sample_is_never_subsumed() {
        let a = rule(NOISE, Scope::program("npm"), 30);
        let b = rule(
            "yarn info fetching package metadata now",
            Scope::program("npm"),
            30,
        );
        let set = set_of(vec![a, b]);
        let samples = vec![npm("nothing relevant\n")];
        assert!(redundant_ids(&audit(&set, &samples, NOW, &AuditOptions::default())).is_empty());
    }

    #[test]
    fn an_old_rule_that_never_fired_is_reported() {
        let young = rule(NOISE, Scope::program("npm"), 3);
        let old = rule(
            "yarn info fetching package metadata now",
            Scope::program("yarn"),
            20,
        );
        let (young_id, old_id) = (young.id.clone(), old.id.clone());
        let set = set_of(vec![young, old]);
        let f = audit(&set, &[], NOW, &AuditOptions::default());
        assert_eq!(kinds(&f, &old_id), vec!["never-hit"]);
        assert!(kinds(&f, &young_id).is_empty());
    }

    #[test]
    fn a_rule_that_stopped_firing_is_stale() {
        let mut r = rule(NOISE, Scope::program("npm"), 200);
        r.hits = 3;
        r.tokens_saved = 90;
        r.last_hit_millis = Some(NOW - 61 * DAY_MILLIS);
        let id = r.id.clone();
        let f = audit(&set_of(vec![r]), &[], NOW, &AuditOptions::default());
        assert_eq!(f.len(), 1, "{f:#?}");
        assert_eq!(f[0].rule, id);
        assert_eq!(f[0].kind, FindingKind::Stale { days: 61 });
    }

    #[test]
    fn a_rule_that_has_not_paid_for_its_lesson_is_reported() {
        let mut r = rule(NOISE, Scope::program("npm"), 2);
        r.teach_tokens = 120;
        r.tokens_saved = 20;
        r.hits = 1;
        r.last_hit_millis = Some(NOW);
        let f = audit(&set_of(vec![r.clone()]), &[], NOW, &AuditOptions::default());
        assert_eq!(f.len(), 1, "{f:#?}");
        assert_eq!(f[0].kind, FindingKind::NegativeNet { net: -100 });

        // Too young and never fired: not judged yet.
        r.hits = 0;
        r.tokens_saved = 0;
        r.last_hit_millis = None;
        assert!(audit(&set_of(vec![r]), &[], NOW, &AuditOptions::default()).is_empty());
    }

    #[test]
    fn a_disabled_rule_is_reported_as_disabled_only() {
        let mut r = rule(NOISE, Scope::program("npm"), 400);
        r.enabled = false;
        let f = audit(
            &set_of(vec![r.clone(), {
                let mut twin = r.clone().with_when(When::Success);
                twin.enabled = true;
                twin.hits = 1;
                twin.tokens_saved = 10;
                twin.last_hit_millis = Some(NOW);
                twin
            }]),
            &[],
            NOW,
            &AuditOptions::default(),
        );
        assert_eq!(kinds(&f, &r.id), vec!["disabled"]);
        assert!(
            redundant_ids(&f).is_empty(),
            "a disabled rule covers nothing"
        );
    }

    #[test]
    fn a_rule_that_eats_most_of_its_output_is_overbroad() {
        let mut r = rule(NOISE, Scope::program("npm"), 1);
        r.hits = 1;
        r.tokens_saved = 10;
        r.last_hit_millis = Some(NOW);
        let id = r.id.clone();
        let heavy = format!("{NOISE}\n{NOISE}\n{NOISE}\n{NOISE}\n{NOISE}\nok\n");
        let set = set_of(vec![r]);
        // One sample is not enough evidence.
        assert!(audit(&set, &[npm(&heavy)], NOW, &AuditOptions::default()).is_empty());
        let f = audit(
            &set,
            &[npm(&heavy), npm(&heavy)],
            NOW,
            &AuditOptions::default(),
        );
        assert_eq!(kinds(&f, &id), vec!["overbroad"]);
        let FindingKind::Overbroad { share } = f[0].kind else {
            panic!("{f:#?}")
        };
        assert!((share - 5.0 / 6.0).abs() < 1e-9);
        // Samples outside the scope do not count.
        let other = Sample::new(Some("yarn"), None, Some(0), heavy.clone());
        assert!(audit(&set, &[npm(&heavy), other], NOW, &AuditOptions::default()).is_empty());
    }

    #[test]
    fn findings_are_ordered_by_rule_id() {
        let a = rule(NOISE, Scope::program("npm"), 100);
        let b = rule(
            "yarn info fetching package metadata now",
            Scope::program("yarn"),
            100,
        );
        let set = set_of(vec![b, a]);
        let f = audit(&set, &[], NOW, &AuditOptions::default());
        let ids: Vec<&str> = f.iter().map(|f| f.rule.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        assert_eq!(ids.len(), 2);
    }
}
