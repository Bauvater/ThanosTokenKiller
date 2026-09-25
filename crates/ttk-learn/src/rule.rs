//! Rules, their scope and the guards that decide whether one may exist.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::pattern::{MAX_BLOCK_LINES, Pattern};

/// Where a rule is allowed to fire.
///
/// `None` in a field means "any". Narrow beats wide: the default scope for a
/// lesson is the exact program and subcommand the output came from, and
/// widening is always an explicit choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Scope {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subcommand: Option<String>,
}

impl Scope {
    /// Fires for every command and for piped `ttk compile` input.
    pub fn global() -> Self {
        Scope::default()
    }

    pub fn program(program: impl Into<String>) -> Self {
        Self {
            program: Some(normalise(program)),
            subcommand: None,
        }
    }

    pub fn command(program: impl Into<String>, subcommand: impl Into<String>) -> Self {
        Self {
            program: Some(normalise(program)),
            // A subcommand is a word, not a path. Running it through
            // `normalise` would turn `/c` into `c` here while the live
            // comparison in `applies_to` still sees `/c`, and the rule would
            // silently never fire.
            subcommand: Some(normalise_subcommand(subcommand)),
        }
    }

    /// Drop the subcommand, keeping the program. `npm install` → `npm`.
    pub fn widened(&self) -> Self {
        Self {
            program: self.program.clone(),
            subcommand: None,
        }
    }

    pub fn is_global(&self) -> bool {
        self.program.is_none() && self.subcommand.is_none()
    }

    /// Does this rule apply to output produced by `program` / `subcommand`?
    ///
    /// A scoped rule never fires on unattributed content (a `ttk compile`
    /// pipe): if we do not know what produced the text, we cannot claim a
    /// command-specific rule is safe for it.
    pub fn applies_to(&self, program: Option<&str>, subcommand: Option<&str>) -> bool {
        match (&self.program, program) {
            (Some(_), None) => return false,
            (Some(want), Some(got)) if !want.eq_ignore_ascii_case(got) => return false,
            _ => {}
        }
        match (&self.subcommand, subcommand) {
            (Some(_), None) => false,
            (Some(want), Some(got)) => want.eq_ignore_ascii_case(got),
            _ => true,
        }
    }

    /// How specific this scope is; used to rank rules for display.
    pub fn specificity(&self) -> u8 {
        self.program.is_some() as u8 + self.subcommand.is_some() as u8
    }

    /// Parse `"npm install"`, `"npm"` or `""` / `"*"` / `"global"`.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s == "*" || s.eq_ignore_ascii_case("global") {
            return Scope::global();
        }
        let mut parts = s.split_whitespace();
        let program = parts.next().map(normalise);
        let subcommand = parts.next().map(normalise_subcommand);
        Scope {
            program,
            subcommand,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.program, &self.subcommand) {
            (Some(p), Some(s)) => write!(f, "{p} {s}"),
            (Some(p), None) => f.write_str(p),
            _ => f.write_str("(any command)"),
        }
    }
}

/// Strip a directory, an `.exe` suffix and the case from a program name so
/// `C:\Program Files\nodejs\NPM.EXE` and `npm` are the same scope.
pub fn normalise(s: impl Into<String>) -> String {
    let s = s.into();
    let base = s
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&s)
        .to_ascii_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_string()
}

/// Lowercase and trim a subcommand, and nothing else.
///
/// Deliberately *not* [`normalise`]: a subcommand is a word, and stripping what
/// looks like a directory from `/c` or `--` would produce a stored scope that
/// no live command can ever match.
pub fn normalise_subcommand(s: impl Into<String>) -> String {
    s.into().trim().to_ascii_lowercase()
}

/// What a rule does with the lines it matches.
///
/// Dropping is the original and the default. The other two exist because
/// "delete it" is not always the right answer:
///
/// * Some noise is worth *counting*. Forty deprecation warnings are noise, but
///   "there were forty deprecation warnings" is a fact an agent may want, and
///   it costs six tokens instead of six hundred.
/// * Some output is 99% noise, and listing what to throw away is hopeless
///   while listing what to keep is one line. Teaching the exception is cheaper
///   than teaching the rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Remove the matched lines.
    #[default]
    Drop,
    /// Replace the matched run with one short line the agent wrote.
    Fold { summary: String },
    /// Keep the matched lines. Everything else in the same scope goes, which
    /// makes this the one rule kind that removes text it never matched — see
    /// [`crate::engine`] for the guards that surround it.
    Keep,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Drop => "drop",
            Action::Fold { .. } => "fold",
            Action::Keep => "keep",
        }
    }

    pub fn is_keep(&self) -> bool {
        matches!(self, Action::Keep)
    }

    /// The line this action leaves behind, if any.
    pub fn folded_line(&self, lines: u64) -> Option<String> {
        match self {
            Action::Fold { summary } => Some(format!("[folded] {summary} ({lines} line(s))")),
            _ => None,
        }
    }
}

/// A learned rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// `flt_<10 hex>`, derived from scope + template, so the same lesson taught
    /// twice is the same rule and importing another project's rules merges
    /// cleanly.
    pub id: String,
    pub scope: Scope,
    /// A single line, or a run of consecutive ones. Serialised as the bare
    /// template it always was when the rule is a line rule, so a rule file
    /// written before block rules existed still loads unchanged.
    pub template: Pattern,
    /// Human readable form of `template`, stored so the file is reviewable
    /// without running anything. Never read back. A block rule renders as one
    /// line per template.
    pub pattern: String,
    /// What happens to the lines this rule matches.
    #[serde(default, flatten)]
    pub action: Action,
    pub created_millis: u64,
    /// How many separate lines this rule was taught from.
    pub examples: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Free text from `--note`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
    /// The capsule the lesson came from, for auditing.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub taught_from: Option<String>,
    /// Learned despite matching an error-shaped line, via `--force`.
    #[serde(default)]
    pub forced: bool,

    // -- accounting, updated by the engine --------------------------------
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub lines_removed: u64,
    #[serde(default)]
    pub tokens_saved: u64,
    #[serde(default)]
    pub last_hit_millis: Option<u64>,
}

fn yes() -> bool {
    true
}

impl Rule {
    pub fn new(scope: Scope, template: Pattern) -> Self {
        Self::with_action(scope, template, Action::Drop)
    }

    pub fn with_action(scope: Scope, template: Pattern, action: Action) -> Self {
        let id = rule_id_for(&scope, &template, &action);
        Self {
            id,
            pattern: template.canonical(),
            action,
            scope,
            template,
            created_millis: ttk_core::ids::now_millis(),
            examples: 1,
            enabled: true,
            note: None,
            taught_from: None,
            forced: false,
            hits: 0,
            lines_removed: 0,
            tokens_saved: 0,
            last_hit_millis: None,
        }
    }

    /// Carry the accounting of `other` into `self` when two rules merge.
    pub fn absorb(&mut self, other: &Rule) {
        self.examples = self.examples.saturating_add(other.examples);
        self.hits += other.hits;
        self.lines_removed += other.lines_removed;
        self.tokens_saved += other.tokens_saved;
        self.created_millis = self.created_millis.min(other.created_millis);
        self.last_hit_millis = match (self.last_hit_millis, other.last_hit_millis) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.enabled = self.enabled && other.enabled;
        self.forced |= other.forced;
        if self.note.is_none() {
            self.note = other.note.clone();
        }
        if self.taught_from.is_none() {
            self.taught_from = other.taught_from.clone();
        }
    }

    /// Does this rule remove exactly this one line?
    ///
    /// False for a block rule even when one of its lines would match: a block
    /// only ever removes a whole run, which is what makes it safe to have
    /// learned lines that are individually meaningless.
    pub fn matches(&self, line: &str) -> bool {
        self.enabled && self.template.matches(line)
    }

    /// Try this rule at `lines[start]`, returning how many lines it removes.
    pub fn match_run(&self, lines: &[&str], start: usize) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        self.template.match_run(lines, start)
    }

    pub fn is_block(&self) -> bool {
        self.template.is_block()
    }

    pub fn is_keep(&self) -> bool {
        self.action.is_keep()
    }

    /// Lines this rule removes at once. `1` for a line rule.
    pub fn height(&self) -> usize {
        self.template.height()
    }
}

/// Deterministic id: same scope and same pattern always give the same rule.
pub fn rule_id(scope: &Scope, template: &Pattern) -> String {
    rule_id_for(scope, template, &Action::Drop)
}

/// The action is part of the identity: folding a line and deleting it are two
/// different instructions about the same text, and teaching one must not
/// silently overwrite the other.
pub fn rule_id_for(scope: &Scope, template: &Pattern, action: &Action) -> String {
    let mut key = format!("{scope}\u{1}{}", template.canonical());
    match action {
        // The original shape, so every id learned before actions existed stays
        // exactly what it was.
        Action::Drop => {}
        Action::Fold { summary } => key.push_str(&format!("\u{1}fold\u{1}{summary}")),
        Action::Keep => key.push_str("\u{1}keep"),
    }
    let hex = blake3::hash(key.as_bytes()).to_hex();
    format!("flt_{}", &hex[..10])
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// Thresholds a candidate template has to clear before it may become a rule.
///
/// These numbers exist to make one specific failure impossible: a template so
/// general that it eats output nobody meant to throw away.
#[derive(Debug, Clone, PartialEq)]
pub struct Guards {
    pub min_literal_tokens: usize,
    pub min_literal_chars: usize,
    /// Highest share of placeholder positions a template may have.
    pub max_placeholder_ratio: f64,
    /// Refuse to learn from a line the invariant extractor considers critical.
    pub protect_errors: bool,
    /// Learn block rules for runs that no line rule could describe.
    pub learn_blocks: bool,
    /// A block has to carry at least this many literal characters *in total*.
    ///
    /// Higher than [`Guards::min_literal_chars`] on purpose: a block removes
    /// several lines at once, so it has to be correspondingly harder to earn.
    pub min_block_literal_chars: usize,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            min_literal_tokens: 2,
            min_literal_chars: 8,
            max_placeholder_ratio: 0.5,
            protect_errors: true,
            learn_blocks: true,
            min_block_literal_chars: 24,
        }
    }
}

/// Why a candidate was refused. Every one of these is reported to the user:
/// silently dropping half a lesson would make the feature feel broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The line had no tokens to build a template from.
    Unusable,
    /// Too few literal tokens or characters left after generalisation.
    TooGeneral { literal_tokens: usize, chars: usize },
    /// More than half the positions are placeholders.
    TooManyPlaceholders { ratio_percent: u32 },
    /// The line carries an exit code, an error type, a comparison, a line
    /// reference or a URL — the same signal the quality firewall protects.
    LooksLikeAnError,
    /// The template would also match a line the lesson said to keep.
    MatchesKeptLine { example: String },
    /// A run of lines too tall to be a block rule.
    BlockTooTall { lines: usize },
    /// Block learning is switched off in the configuration.
    BlocksDisabled,
    /// A `<filter-fold>` block with no `as="…"` caption.
    FoldWithoutSummary,
}

impl Rejection {
    pub fn summary(&self) -> String {
        match self {
            Rejection::Unusable => "nothing to match on".to_string(),
            Rejection::TooGeneral {
                literal_tokens,
                chars,
            } => format!("too general ({literal_tokens} literal token(s), {chars} char(s))"),
            Rejection::TooManyPlaceholders { ratio_percent } => {
                format!("{ratio_percent}% of the pattern is placeholders")
            }
            Rejection::LooksLikeAnError => {
                "looks like an error, and errors are never filtered (use --force)".to_string()
            }
            Rejection::MatchesKeptLine { example } => {
                format!("would also remove a line you kept: {example}")
            }
            Rejection::BlockTooTall { lines } => {
                format!("{lines} lines is taller than a block rule may be ({MAX_BLOCK_LINES})")
            }
            Rejection::BlocksDisabled => {
                "too general on its own, and learning.learn_blocks is off".to_string()
            }
            Rejection::FoldWithoutSummary => {
                "a fold needs a caption: <filter-fold as=\"what this run amounts to\">".to_string()
            }
        }
    }

    /// Would a *block* rule plausibly rescue a line this rejection refused?
    ///
    /// Only generality rejections qualify. An error-shaped line is never
    /// rescued by being surrounded with more lines, and neither is a line that
    /// matches something the lesson explicitly kept.
    pub fn is_generality(&self) -> bool {
        matches!(
            self,
            Rejection::TooGeneral { .. } | Rejection::TooManyPlaceholders { .. }
        )
    }
}

/// Does this line carry signal the quality firewall would defend?
///
/// Reuses `ttk_core::invariants::critical_regions`, so "what counts as an
/// error" has exactly one definition in the codebase and a learned filter can
/// never disagree with the firewall about it.
pub fn looks_critical(line: &str) -> bool {
    !ttk_core::invariants::critical_regions(line)
        .trim()
        .is_empty()
}

/// Check one candidate pattern against the guards and the keep set.
///
/// `source` is the line (or, for a block, the lines) the pattern was built
/// from; it is what the error guard inspects, because a template has already
/// thrown away the values that make a line recognisably an error.
pub fn check(
    pattern: &Pattern,
    source: &[String],
    keep: &[String],
    guards: &Guards,
    force: bool,
) -> Result<(), Rejection> {
    if pattern.is_empty() {
        return Err(Rejection::Unusable);
    }
    if pattern.height() > MAX_BLOCK_LINES {
        return Err(Rejection::BlockTooTall {
            lines: pattern.height(),
        });
    }
    if guards.protect_errors && !force && source.iter().any(|line| looks_critical(line)) {
        return Err(Rejection::LooksLikeAnError);
    }

    let literal_tokens = pattern.literal_tokens();
    let chars = pattern.literal_chars();
    // A block is judged on its total substance rather than per line: that is
    // the whole reason it exists, since its individual lines are what a line
    // rule already refused.
    let min_chars = if pattern.is_block() {
        guards.min_block_literal_chars
    } else {
        guards.min_literal_chars
    };
    if literal_tokens < guards.min_literal_tokens || chars < min_chars {
        return Err(Rejection::TooGeneral {
            literal_tokens,
            chars,
        });
    }

    let placeholders = pattern.len() - literal_tokens;
    let ratio = placeholders as f64 / pattern.len() as f64;
    if ratio > guards.max_placeholder_ratio {
        return Err(Rejection::TooManyPlaceholders {
            ratio_percent: (ratio * 100.0).round() as u32,
        });
    }

    if let Some(hit) = first_kept_match(pattern, keep) {
        return Err(Rejection::MatchesKeptLine { example: hit });
    }
    Ok(())
}

/// The first thing in the keep set this pattern would have removed, if any.
///
/// For a line pattern that is a plain scan. For a block it is a scan over every
/// window of the kept lines: the kept lines are in document order, so a block
/// that matches a run of them is a block that would have eaten text the lesson
/// explicitly left alone.
fn first_kept_match(pattern: &Pattern, keep: &[String]) -> Option<String> {
    if !pattern.is_block() {
        return keep
            .iter()
            .find(|k| pattern.matches(k))
            .map(|k| k.trim().to_string());
    }
    let lines: Vec<&str> = keep.iter().map(String::as_str).collect();
    (0..lines.len())
        .find(|&i| pattern.match_run(&lines, i).is_some())
        .map(|i| {
            let last = (i + pattern.height() - 1).min(lines.len() - 1);
            format!("{} … {}", lines[i].trim(), lines[last].trim())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pattern::Template;

    fn tpl(line: &str) -> Template {
        Template::from_line(line).expect("template")
    }

    fn pat(line: &str) -> Pattern {
        Pattern::line(tpl(line))
    }

    fn block(lines: &[&str]) -> Pattern {
        Pattern::block(lines.iter().map(|l| tpl(l)).collect()).expect("block")
    }

    fn src(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_string()).collect()
    }

    #[test]
    fn scope_matching_is_narrow_by_default() {
        let s = Scope::command("npm", "install");
        assert!(s.applies_to(Some("npm"), Some("install")));
        assert!(!s.applies_to(Some("npm"), Some("test")));
        assert!(!s.applies_to(Some("npm"), None));
        assert!(!s.applies_to(None, None));

        let p = Scope::program("npm");
        assert!(p.applies_to(Some("npm"), Some("install")));
        assert!(p.applies_to(Some("npm"), None));
        assert!(!p.applies_to(Some("yarn"), None));

        let g = Scope::global();
        assert!(g.applies_to(None, None));
        assert!(g.applies_to(Some("anything"), Some("at-all")));
        assert!(g.is_global());
    }

    #[test]
    fn scope_normalises_program_names() {
        let s = Scope::program(r"C:\Program Files\nodejs\NPM.EXE");
        assert_eq!(s.to_string(), "npm");
        assert!(s.applies_to(Some("npm"), None));
        assert_eq!(
            Scope::parse("npm install"),
            Scope::command("npm", "install")
        );
        assert_eq!(Scope::parse("  "), Scope::global());
        assert_eq!(Scope::parse("global"), Scope::global());
        assert_eq!(
            Scope::command("npm", "install").widened(),
            Scope::program("npm")
        );
    }

    /// A subcommand must survive being stored: it is compared against the raw
    /// argv token later, so anything the constructor strips is a rule that can
    /// never fire.
    #[test]
    fn a_subcommand_is_not_treated_as_a_path() {
        let s = Scope::command("cmd", "/c");
        assert_eq!(s.subcommand.as_deref(), Some("/c"));
        assert!(s.applies_to(Some("cmd"), Some("/c")));
        assert_eq!(Scope::parse("cmd /C").subcommand.as_deref(), Some("/c"));
        assert!(Scope::parse("cmd /C").applies_to(Some("cmd"), Some("/c")));
    }

    #[test]
    fn an_action_is_part_of_a_rules_identity() {
        let t = pat("npm WARN deprecated glob@7.2.3");
        let drop = Rule::new(Scope::global(), t.clone());
        let fold = Rule::with_action(
            Scope::global(),
            t.clone(),
            Action::Fold {
                summary: "npm noise".into(),
            },
        );
        let keep = Rule::with_action(Scope::global(), t, Action::Keep);
        assert_ne!(drop.id, fold.id);
        assert_ne!(drop.id, keep.id);
        assert_ne!(fold.id, keep.id);
        // A plain drop keeps the id it always had.
        assert_eq!(drop.id, rule_id(&Scope::global(), &drop.template));
    }

    #[test]
    fn a_fold_leaves_one_line_behind() {
        let action = Action::Fold {
            summary: "npm: deprecation warnings".into(),
        };
        assert_eq!(
            action.folded_line(40).as_deref(),
            Some("[folded] npm: deprecation warnings (40 line(s))")
        );
        assert_eq!(Action::Drop.folded_line(40), None);
        assert_eq!(Action::Keep.folded_line(40), None);
    }

    #[test]
    fn rule_ids_are_deterministic_and_scope_aware() {
        let t = pat("npm WARN deprecated glob@7.2.3");
        let a = rule_id(&Scope::program("npm"), &t);
        assert_eq!(a, rule_id(&Scope::program("npm"), &t));
        assert_ne!(a, rule_id(&Scope::global(), &t));
        assert!(a.starts_with("flt_"));
        assert_eq!(a.len(), 14);
    }

    #[test]
    fn error_shaped_lines_are_refused() {
        let line = "FAILED tests/a.py::test_x - assert 200 == 401";
        let t = pat(line);
        assert!(looks_critical(line));
        assert_eq!(
            check(&t, &src(&[line]), &[], &Guards::default(), false),
            Err(Rejection::LooksLikeAnError)
        );
        // …unless the user insists, which is recorded on the rule.
        assert!(check(&t, &src(&[line]), &[], &Guards::default(), true).is_ok());
    }

    /// One error anywhere in a run poisons the whole block. Wrapping a failure
    /// line in decoration must not be a way to get it deleted.
    #[test]
    fn one_error_in_a_run_refuses_the_whole_block() {
        let lines = [
            "======================================",
            "FAILED tests/a.py::test_x - assert 200 == 401",
            "======================================",
        ];
        let b = block(&lines);
        assert_eq!(
            check(&b, &src(&lines), &[], &Guards::default(), false),
            Err(Rejection::LooksLikeAnError)
        );
    }

    /// The point of block rules: lines that are individually unlearnable are
    /// learnable together.
    #[test]
    fn a_banner_is_refused_line_by_line_but_accepted_as_a_block() {
        let lines = [
            "======================================",
            "  Welcome to the Acme build system",
            "======================================",
        ];
        let guards = Guards::default();
        // The decoration line on its own is far too generic.
        let err =
            check(&pat(lines[0]), &src(&[lines[0]]), &[], &guards, false).expect_err("must reject");
        assert!(err.is_generality(), "{err:?}");
        // The three of them together carry plenty of signal.
        check(&block(&lines), &src(&lines), &[], &guards, true).expect("block is learnable");
    }

    #[test]
    fn a_block_that_would_eat_kept_text_is_refused() {
        let lines = [
            "======================================",
            "  Welcome to the Acme build system",
            "======================================",
        ];
        let keep = src(&lines);
        let err = check(
            &block(&lines),
            &src(&lines),
            &keep,
            &Guards::default(),
            false,
        )
        .expect_err("must reject");
        assert!(matches!(err, Rejection::MatchesKeptLine { .. }), "{err:?}");
        assert!(err.summary().contains("Welcome") || err.summary().contains("==="));
    }

    #[test]
    fn a_thin_block_is_still_refused() {
        let lines = ["== a ==", "== b =="];
        let err = check(&block(&lines), &src(&lines), &[], &Guards::default(), false)
            .expect_err("must reject");
        assert!(
            matches!(err, Rejection::TooGeneral { .. }),
            "a block earns a higher bar, not a lower one: {err:?}"
        );
    }

    #[test]
    fn a_rule_may_not_match_something_you_kept() {
        let line = "Downloading package alpha from cache";
        let t = tpl(line)
            .merge(&tpl("Downloading package beta from cache"))
            .expect("merged");
        let keep = vec!["Downloading package critical from cache".to_string()];
        let err = check(
            &Pattern::line(t),
            &src(&[line]),
            &keep,
            &Guards::default(),
            false,
        )
        .expect_err("must reject");
        assert!(matches!(err, Rejection::MatchesKeptLine { .. }));
        assert!(err.summary().contains("critical"));
    }

    #[test]
    fn thin_patterns_are_refused() {
        let line = "ok 12";
        let err = check(&pat(line), &src(&[line]), &[], &Guards::default(), false)
            .expect_err("must reject");
        assert!(matches!(err, Rejection::TooGeneral { .. }), "{err:?}");
        assert!(err.is_generality());
    }

    #[test]
    fn placeholder_heavy_patterns_are_refused() {
        let line = "processing 3 of 5 at 200ms 9f1c2d3e4 2026-09-05";
        let err = check(&pat(line), &src(&[line]), &[], &Guards::default(), false)
            .expect_err("must reject");
        assert!(
            matches!(err, Rejection::TooManyPlaceholders { .. }),
            "{err:?}"
        );
        assert!(err.is_generality());
    }

    #[test]
    fn a_normal_noise_line_passes_every_guard() {
        let line = "npm WARN deprecated inflight@1.0.6: This module is not supported";
        check(
            &pat(line),
            &src(&[line]),
            &["added 412 packages in 9s".into()],
            &Guards::default(),
            false,
        )
        .expect("must be learnable");
    }

    #[test]
    fn a_block_rule_only_ever_removes_a_whole_run() {
        let lines = [
            "======================================",
            "  Welcome to the Acme build system",
            "======================================",
        ];
        let rule = Rule::new(Scope::global(), block(&lines));
        assert!(rule.is_block());
        assert_eq!(rule.height(), 3);
        // A single line of the block is never removed on its own…
        assert!(!rule.matches(lines[0]));
        // …but the run is.
        let text: Vec<&str> = lines.to_vec();
        assert_eq!(rule.match_run(&text, 0), Some(3));
        assert_eq!(rule.match_run(&text, 1), None);
        // A `<filter-keep>` on any of its lines still points at this rule.
        assert!(rule.template.any_line_matches(lines[1]));
    }

    #[test]
    fn absorbing_keeps_the_accounting() {
        let t = pat("npm WARN deprecated glob@7.2.3 is old");
        let mut a = Rule::new(Scope::program("npm"), t.clone());
        a.hits = 3;
        a.tokens_saved = 30;
        let mut b = Rule::new(Scope::program("npm"), t);
        b.hits = 4;
        b.tokens_saved = 40;
        b.enabled = false;
        a.absorb(&b);
        assert_eq!(a.hits, 7);
        assert_eq!(a.tokens_saved, 70);
        assert_eq!(a.examples, 2);
        assert!(
            !a.enabled,
            "a disabled twin must not be re-enabled by a merge"
        );
    }
}
