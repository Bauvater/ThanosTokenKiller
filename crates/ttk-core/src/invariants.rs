//! Invariants: the facts a transformation is not allowed to lose.
//!
//! An [`Invariant`] is always a *verbatim substring* of the original content.
//! That makes verification exact and cheap: the quality firewall only has to
//! check that the string still occurs in the transformed output. There is no
//! fuzzy matching and therefore no way for a "close enough" rewrite to pass.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantKind {
    /// `src/auth/token.rs`, `C:\project\x.rs`
    Path,
    /// `src/auth/token.rs:118`
    LineRef,
    Url,
    /// git sha, sha256, blake3 digests
    Hash,
    /// `1.2.3`, `v0.4.0-rc1`
    Version,
    /// A shell command line.
    Command,
    /// `exit code 2`
    ExitCode,
    /// `AssertionError`, `E0433`, `TS2345`
    ErrorType,
    /// A bare number with or without a unit.
    Number,
    /// `expected=401` / `actual=200` pairs.
    Comparison,
    /// Negations that flip meaning.
    Negation,
    /// Explicitly marked security relevant text.
    SecurityNote,
    /// Anything a compiler declares by hand.
    Custom,
}

impl InvariantKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InvariantKind::Path => "path",
            InvariantKind::LineRef => "line_ref",
            InvariantKind::Url => "url",
            InvariantKind::Hash => "hash",
            InvariantKind::Version => "version",
            InvariantKind::Command => "command",
            InvariantKind::ExitCode => "exit_code",
            InvariantKind::ErrorType => "error_type",
            InvariantKind::Number => "number",
            InvariantKind::Comparison => "comparison",
            InvariantKind::Negation => "negation",
            InvariantKind::SecurityNote => "security_note",
            InvariantKind::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Invariant {
    pub kind: InvariantKind,
    /// Verbatim substring of the original content.
    pub value: String,
}

impl Invariant {
    pub fn new(kind: InvariantKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }

    pub fn custom(value: impl Into<String>) -> Self {
        Self::new(InvariantKind::Custom, value)
    }

    /// Does the invariant still occur in `text`?
    pub fn holds_in(&self, text: &str) -> bool {
        self.value.is_empty() || text.contains(self.value.as_str())
    }
}

/// Which invariant kinds to extract, and how many per kind.
#[derive(Debug, Clone)]
pub struct ExtractPolicy {
    pub kinds: Vec<InvariantKind>,
    /// Hard cap per kind so pathological input cannot blow up memory.
    pub max_per_kind: usize,
    /// Only inspect the first N bytes.
    pub max_bytes: usize,
}

impl Default for ExtractPolicy {
    fn default() -> Self {
        Self {
            kinds: vec![
                InvariantKind::LineRef,
                InvariantKind::Path,
                InvariantKind::Url,
                InvariantKind::Hash,
                InvariantKind::Version,
                InvariantKind::ExitCode,
                InvariantKind::ErrorType,
                InvariantKind::Comparison,
            ],
            max_per_kind: 512,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

impl ExtractPolicy {
    /// The strict set used for error-shaped content: nothing here may vanish.
    pub fn critical() -> Self {
        Self {
            kinds: vec![
                InvariantKind::LineRef,
                InvariantKind::ExitCode,
                InvariantKind::ErrorType,
                InvariantKind::Comparison,
                InvariantKind::Hash,
            ],
            ..Self::default()
        }
    }
}

pub static LOG_LEVEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(trace|debug|info|notice|warn|warning|error|err|fatal|critical|panic)\b")
        .expect("static regex")
});

static LINE_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:[A-Za-z]:)?[\w./\\+@-]+\.[A-Za-z][A-Za-z0-9]{0,9}:\d+(?::\d+)?")
        .expect("static regex")
});
static PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:[A-Za-z]:[\\/])?(?:[\w.+@-]+[\\/])+[\w.+@-]+").expect("static regex")
});
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s)\]'"*,]+"#).expect("static regex")
});
static HASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{7,64}\b").expect("static regex"));
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bv?\d+\.\d+(?:\.\d+)?(?:-[0-9A-Za-z.-]+)?\b").expect("static regex")
});
static EXIT_CODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:exit(?:ed)?[ _-]?(?:code|status)?|status)[ =:]+(-?\d+)\b")
        .expect("static regex")
});
static ERROR_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:[A-Z][a-zA-Z0-9]*(?:Error|Exception|Panic|Failure|Fault)|E\d{3,4}|TS\d{4}|C\d{4})\b",
    )
    .expect("static regex")
});
static COMPARISON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:expected|actual|got|want|left|right)\b\s*[:=]?\s*[^\s,;]{1,64}")
        .expect("static regex")
});
static NEGATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:not|never|no|without|cannot|can't|don't|doesn't|must not|failed to)\b")
        .expect("static regex")
});
static SECURITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:vulnerab\w+|CVE-\d{4}-\d+|security|unsafe|injection|exploit|privilege|secret|credential)\b")
        .expect("static regex")
});
static NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d+(?:\.\d+)?\s?(?:ms|s|m|h|kb|mb|gb|tb|%|bytes|files|tests)?\b")
        .expect("static regex")
});

fn re_for(kind: InvariantKind) -> Option<&'static Regex> {
    Some(match kind {
        InvariantKind::LineRef => &LINE_REF_RE,
        InvariantKind::Path => &PATH_RE,
        InvariantKind::Url => &URL_RE,
        InvariantKind::Hash => &HASH_RE,
        InvariantKind::Version => &VERSION_RE,
        InvariantKind::ExitCode => &EXIT_CODE_RE,
        InvariantKind::ErrorType => &ERROR_TYPE_RE,
        InvariantKind::Comparison => &COMPARISON_RE,
        InvariantKind::Negation => &NEGATION_RE,
        InvariantKind::SecurityNote => &SECURITY_RE,
        InvariantKind::Number => &NUMBER_RE,
        InvariantKind::Command | InvariantKind::Custom => return None,
    })
}

/// Extract invariants from `text` according to `policy`.
///
/// The result is deduplicated and sorted, so it is stable across runs — a
/// requirement for golden tests and for replay.
pub fn extract(text: &str, policy: &ExtractPolicy) -> Vec<Invariant> {
    let slice = truncate_on_char_boundary(text, policy.max_bytes);
    let mut set: BTreeSet<Invariant> = BTreeSet::new();

    for &kind in &policy.kinds {
        let Some(re) = re_for(kind) else { continue };
        let mut n = 0usize;
        for m in re.find_iter(slice) {
            if n >= policy.max_per_kind {
                break;
            }
            let value = m.as_str();
            if !is_useful(kind, value) {
                continue;
            }
            set.insert(Invariant::new(kind, value));
            n += 1;
        }
    }

    // A line reference already contains its path; keep the more specific one.
    let line_refs: Vec<String> = set
        .iter()
        .filter(|i| i.kind == InvariantKind::LineRef)
        .map(|i| i.value.clone())
        .collect();
    set.retain(|i| {
        i.kind != InvariantKind::Path || !line_refs.iter().any(|lr| lr.starts_with(&i.value))
    });

    set.into_iter().collect()
}

fn is_useful(kind: InvariantKind, value: &str) -> bool {
    match kind {
        // A "path" needs a separator and must not be a bare number pair.
        InvariantKind::Path => {
            value.contains('/') || value.contains('\\') || value.matches('.').count() >= 1
        }
        // Very short hex runs are usually just numbers.
        InvariantKind::Hash => value.len() >= 7 && value.chars().any(|c| c.is_ascii_alphabetic()),
        InvariantKind::Number => value.len() <= 24,
        _ => !value.is_empty(),
    }
}

fn truncate_on_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

static CRITICAL_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        // `err!` is the odd one out and it earns its place: npm, yarn and pnpm
        // all print `npm ERR!` rather than the word "error", so without it the
        // single most common failure line in the JavaScript world is invisible
        // to every guard that asks "is this evidence?".
        r"(?i)(\b(error|failed|failure|fail:|panic|panicked|exception|traceback|assert\w*|fatal|cannot|denied|refused|timed?[ _-]?out|expected|actual|exit(?:ed)?[ _-]?(?:code|status))\b|\bERR!)",
    )
    .expect("static regex")
});

/// The lines of `text` that carry error-shaped information.
///
/// The quality firewall derives its *mandatory* invariants from this subset
/// rather than from the whole input. Requiring every path in a 50k line log to
/// survive would make compression impossible; requiring every path mentioned
/// on a failing line is exactly the guarantee that matters.
pub fn critical_regions(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if CRITICAL_LINE_RE.is_match(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Invariants of `original` that no longer occur in `transformed`.
pub fn violations(declared: &[Invariant], transformed: &str) -> Vec<Invariant> {
    declared
        .iter()
        .filter(|i| !i.holds_in(transformed))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `npm ERR!` is a failure line that contains no word this pass would
    /// otherwise recognise.
    #[test]
    fn npm_style_error_markers_count_as_critical() {
        let text = "npm ERR! code ELIFECYCLE
added 412 packages in 9s
";
        let critical = critical_regions(text);
        assert!(critical.contains("npm ERR!"), "{critical}");
        assert!(!critical.contains("added 412"), "{critical}");
        // …and an ordinary word that merely starts the same way is not.
        assert!(critical_regions("erratic behaviour is fine here").is_empty());
    }

    #[test]
    fn extracts_line_refs_and_error_types() {
        let text = "F1 tests/auth/test_expiry.py:87\nAssertionError expected=401 actual=200";
        let inv = extract(text, &ExtractPolicy::critical());
        assert!(
            inv.iter()
                .any(|i| i.value == "tests/auth/test_expiry.py:87")
        );
        assert!(inv.iter().any(|i| i.value == "AssertionError"));
        assert!(inv.iter().any(|i| i.value.starts_with("expected")));
    }

    #[test]
    fn line_ref_supersedes_plain_path() {
        let inv = extract("src/a.rs:12 failed", &ExtractPolicy::default());
        assert!(inv.iter().any(|i| i.kind == InvariantKind::LineRef));
        assert!(
            !inv.iter()
                .any(|i| i.kind == InvariantKind::Path && i.value == "src/a.rs")
        );
    }

    #[test]
    fn violations_are_detected() {
        let declared = extract("boom at src/x.rs:9", &ExtractPolicy::critical());
        assert!(violations(&declared, "boom at src/x.rs:9 (trimmed)").is_empty());
        assert_eq!(
            violations(&declared, "something else").len(),
            declared.len()
        );
    }

    #[test]
    fn extraction_is_deterministic() {
        let text = "a/b.rs:1 c/d.rs:2 https://example.com/x v1.2.3 exit code 3";
        assert_eq!(
            extract(text, &ExtractPolicy::default()),
            extract(text, &ExtractPolicy::default())
        );
    }

    #[test]
    fn extraction_is_bounded() {
        let policy = ExtractPolicy {
            max_per_kind: 4,
            ..ExtractPolicy::default()
        };
        let text = (0..1000)
            .map(|i| format!("f{i}.rs:{i}\n"))
            .collect::<String>();
        let inv = extract(&text, &policy);
        assert!(
            inv.iter()
                .filter(|i| i.kind == InvariantKind::LineRef)
                .count()
                <= 4
        );
    }

    #[test]
    fn windows_paths_are_recognised() {
        let inv = extract(r"C:\work\src\main.rs:42: error", &ExtractPolicy::critical());
        assert!(
            inv.iter().any(|i| i.value.ends_with("main.rs:42")),
            "got {inv:?}"
        );
    }

    #[test]
    fn critical_regions_keeps_only_error_shaped_lines() {
        let text = "ok: everything fine\n\
                    downloading package a\n\
                    ERROR: cannot open src/a.rs:12\n\
                    downloading package b\n\
                    test failed at tests/b.rs:9\n";
        let critical = critical_regions(text);
        assert!(critical.contains("src/a.rs:12"));
        assert!(critical.contains("tests/b.rs:9"));
        assert!(!critical.contains("downloading package"));

        // …and those are exactly the lines the firewall derives from.
        let inv = extract(&critical, &ExtractPolicy::critical());
        assert!(inv.iter().any(|i| i.value == "src/a.rs:12"));
        assert!(inv.iter().any(|i| i.value == "tests/b.rs:9"));
    }

    #[test]
    fn critical_regions_of_clean_output_is_empty() {
        assert!(critical_regions("all good\n2 files changed\n").is_empty());
    }

    #[test]
    fn urls_survive_extraction() {
        let inv = extract(
            "see https://example.test/a?b=1 for details",
            &ExtractPolicy::default(),
        );
        assert!(inv.iter().any(|i| i.value == "https://example.test/a?b=1"));
    }
}
