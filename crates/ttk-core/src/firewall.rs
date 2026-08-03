//! The quality firewall.
//!
//! Every transformation has to pass through [`Firewall::review`]. The firewall
//! is the only place allowed to decide that a transformed output may replace
//! the original, and it errs on the side of the original:
//!
//! 1. declared invariants must still occur verbatim in the output,
//! 2. structural outputs must re-parse,
//! 3. the transformation must actually save tokens,
//! 4. anything unexpected (including a panic) degrades to pass-through.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::config::{Config, Mode};
use crate::event::{FallbackReason, TransformationRecord, ValidationOutcome};
use crate::invariants::{self, ExtractPolicy, Invariant, InvariantKind};
use crate::ir;
use crate::tokens::{self, TokenCount};

/// Extra structural check to run on the candidate output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reparse {
    /// No structural guarantee claimed.
    None,
    /// Output must be a valid Token IR document.
    TokenIr,
    /// Output must be JSON that is semantically equal to the original.
    JsonEquivalent,
    /// Output must be valid JSON (structure may differ, e.g. a summary).
    JsonValid,
}

/// What a compiler proposes.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub transformer: String,
    pub version: u32,
    pub output: String,
    /// Invariants the compiler guarantees to have preserved.
    pub invariants: Vec<Invariant>,
    pub reparse: Reparse,
    pub notes: Vec<String>,
    /// Compiler flagged reduced fidelity (e.g. clustered log lines).
    pub lossy: bool,
}

impl Candidate {
    pub fn new(transformer: impl Into<String>, version: u32, output: String) -> Self {
        Self {
            transformer: transformer.into(),
            version,
            output,
            invariants: Vec::new(),
            reparse: Reparse::None,
            notes: Vec::new(),
            lossy: false,
        }
    }

    pub fn invariants(mut self, inv: Vec<Invariant>) -> Self {
        self.invariants = inv;
        self
    }

    pub fn reparse(mut self, r: Reparse) -> Self {
        self.reparse = r;
        self
    }

    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.notes.push(n.into());
        self
    }

    pub fn lossy(mut self, lossy: bool) -> Self {
        self.lossy = lossy;
        self
    }
}

/// Result of a firewall review. `output` is what the caller must use — it is
/// the original whenever the candidate was rejected.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub output: String,
    pub record: TransformationRecord,
}

impl Verdict {
    pub fn accepted(&self) -> bool {
        matches!(
            self.record.validation,
            ValidationOutcome::Passed | ValidationOutcome::PassedWithWarnings
        )
    }
}

pub struct Firewall<'a> {
    config: &'a Config,
}

impl<'a> Firewall<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    /// Invariants the *configuration* insists on, on top of what the compiler
    /// declared. Extracted from the original content.
    fn mandatory_invariants(&self, original: &str) -> Vec<Invariant> {
        let q = &self.config.quality;
        let mut kinds = vec![
            InvariantKind::ExitCode,
            InvariantKind::ErrorType,
            InvariantKind::Comparison,
        ];
        if q.preserve_line_numbers {
            kinds.push(InvariantKind::LineRef);
        }
        if q.preserve_urls {
            kinds.push(InvariantKind::Url);
        }
        if self.config.mode == Mode::Forensic || self.config.mode == Mode::Debug {
            kinds.push(InvariantKind::Hash);
        }
        // Only error-shaped lines contribute mandatory invariants; see
        // [`invariants::critical_regions`] for why.
        let critical = invariants::critical_regions(original);
        invariants::extract(
            &critical,
            &ExtractPolicy {
                kinds,
                max_per_kind: 128,
                ..ExtractPolicy::critical()
            },
        )
    }

    /// Review `candidate` against `original`.
    pub fn review(&self, original: &str, candidate: Candidate) -> Verdict {
        let before = tokens::estimate(original);
        let after = tokens::estimate(&candidate.output);
        let input_hash = crate::hash_string(original);

        let reject = |reason: FallbackReason, notes: Vec<String>| Verdict {
            output: original.to_string(),
            record: TransformationRecord {
                transformer: candidate.transformer.clone(),
                version: candidate.version,
                at_millis: crate::ids::now_millis(),
                input_hash: input_hash.clone(),
                output_hash: input_hash.clone(),
                tokens_before: before,
                tokens_after: before,
                invariants: Vec::new(),
                validation: ValidationOutcome::Failed,
                fallback: Some(reason),
                notes,
            },
        };

        // 1. Mode gate.
        if !self.config.mode.transforms() {
            return Verdict {
                output: original.to_string(),
                record: TransformationRecord {
                    transformer: candidate.transformer,
                    version: candidate.version,
                    at_millis: crate::ids::now_millis(),
                    input_hash: input_hash.clone(),
                    output_hash: input_hash,
                    tokens_before: before,
                    tokens_after: before,
                    invariants: Vec::new(),
                    validation: ValidationOutcome::Skipped,
                    fallback: Some(FallbackReason::PolicyDisabled("observe mode".into())),
                    notes: vec![format!(
                        "observe mode: would have saved {} tokens",
                        before.saved_against(after)
                    )],
                },
            };
        }

        // 2. Lossy transformations need an explicit mode.
        if candidate.lossy && !self.config.mode.allows_lossy() {
            return reject(
                FallbackReason::PolicyDisabled(format!(
                    "lossy transformation not allowed in {} mode",
                    self.config.mode
                )),
                candidate.notes.clone(),
            );
        }

        // 3. Invariants.
        let mut declared = candidate.invariants.clone();
        for inv in self.mandatory_invariants(original) {
            if !declared.contains(&inv) {
                declared.push(inv);
            }
        }
        declared.sort();
        declared.dedup();
        let lost = invariants::violations(&declared, &candidate.output);
        if !lost.is_empty() {
            return reject(FallbackReason::InvariantLost(lost), candidate.notes.clone());
        }

        // 4. Structural re-parse.
        if let Err(e) = self.check_reparse(original, &candidate) {
            return reject(FallbackReason::ReparseFailed(e), candidate.notes.clone());
        }

        // 5. The transformation has to pay for itself.
        let min_gain = self.config.quality.min_relative_gain;
        let threshold = (before.value as f64 * (1.0 - min_gain)).floor() as u64;
        if before.value > 0 && after.value > threshold {
            return reject(FallbackReason::NoGain, candidate.notes.clone());
        }

        Verdict {
            output: candidate.output.clone(),
            record: TransformationRecord {
                transformer: candidate.transformer,
                version: candidate.version,
                at_millis: crate::ids::now_millis(),
                input_hash,
                output_hash: crate::hash_string(&candidate.output),
                tokens_before: before,
                tokens_after: after,
                invariants: declared,
                validation: if candidate.lossy {
                    ValidationOutcome::PassedWithWarnings
                } else {
                    ValidationOutcome::Passed
                },
                fallback: None,
                notes: candidate.notes,
            },
        }
    }

    fn check_reparse(&self, original: &str, candidate: &Candidate) -> Result<(), String> {
        match candidate.reparse {
            Reparse::None => Ok(()),
            Reparse::TokenIr => ir::parse(&candidate.output)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Reparse::JsonValid => serde_json::from_str::<serde_json::Value>(&candidate.output)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Reparse::JsonEquivalent => {
                let a: serde_json::Value = serde_json::from_str(original)
                    .map_err(|e| format!("original is not JSON: {e}"))?;
                let b: serde_json::Value = serde_json::from_str(&candidate.output)
                    .map_err(|e| format!("output is not JSON: {e}"))?;
                if a == b {
                    Ok(())
                } else {
                    Err("JSON output is not semantically equal to the input".to_string())
                }
            }
        }
    }
}

thread_local! {
    /// Set while a guarded compiler runs on *this* thread.
    static SILENCE_PANICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install a panic hook that stays quiet only for guarded compiler calls.
///
/// The hook is global, so it must not be swapped in and out around every call:
/// that would swallow unrelated panics from other threads. Instead it is
/// installed once and consults a thread local flag.
fn install_quiet_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !SILENCE_PANICS.with(|s| s.get()) {
                previous(info);
            }
        }));
    });
}

/// Run a compiler, converting a panic into a rejected candidate.
///
/// A parser bug must never take down an agent run.
pub fn guarded<F>(
    transformer: &str,
    version: u32,
    original: &str,
    f: F,
) -> Result<Candidate, Box<Verdict>>
where
    F: FnOnce() -> Candidate,
{
    install_quiet_hook();
    let result = SILENCE_PANICS.with(|s| {
        s.set(true);
        let r = catch_unwind(AssertUnwindSafe(f));
        s.set(false);
        r
    });

    result.map_err(|payload| {
        let msg = panic_message(&payload);
        // Boxed: this is the cold path and `Verdict` is large.
        let before = tokens::estimate(original);
        let hash = crate::hash_string(original);
        Box::new(Verdict {
            output: original.to_string(),
            record: TransformationRecord {
                transformer: transformer.to_string(),
                version,
                at_millis: crate::ids::now_millis(),
                input_hash: hash.clone(),
                output_hash: hash,
                tokens_before: before,
                tokens_after: before,
                invariants: Vec::new(),
                validation: ValidationOutcome::Failed,
                fallback: Some(FallbackReason::TransformerError(msg)),
                notes: vec!["fell back to unmodified pass-through".to_string()],
            },
        })
    })
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic: <non-string payload>".to_string()
    }
}

/// Convenience for reporting: how much a verdict actually saved.
pub fn verdict_saving(v: &Verdict) -> TokenCount {
    v.record.tokens_before.saved_against(v.record.tokens_after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg(mode: Mode) -> Config {
        Config {
            mode,
            ..Config::default()
        }
    }

    const ORIGINAL: &str = "running tests\n\
        lots and lots of irrelevant chatter that nobody needs to read at all\n\
        more chatter, even more chatter, filler filler filler filler filler\n\
        FAILED tests/auth/test_expiry.py:87 - AssertionError expected=401 actual=200\n\
        done, exit code 1\n";

    fn good_candidate() -> Candidate {
        Candidate::new(
            "test.demo",
            1,
            "[test:demo] status=fail\n\
             failed=1\n\
             @f at=tests/auth/test_expiry.py:87\n\
             \x20 AssertionError expected=401 actual=200\n\
             \x20 exit code 1\n"
                .to_string(),
        )
        .reparse(Reparse::TokenIr)
    }

    #[test]
    fn accepts_a_lossless_compression() {
        let c = cfg(Mode::Safe);
        let v = Firewall::new(&c).review(ORIGINAL, good_candidate());
        assert!(v.accepted(), "{:?}", v.record.fallback);
        assert!(v.record.tokens_after.value < v.record.tokens_before.value);
        assert!(v.output.starts_with("[test:demo]"));
    }

    #[test]
    fn rejects_when_an_invariant_disappears() {
        let c = cfg(Mode::Safe);
        let candidate = Candidate::new("test.demo", 1, "[test:demo] status=fail\n".to_string())
            .reparse(Reparse::TokenIr);
        let v = Firewall::new(&c).review(ORIGINAL, candidate);
        assert!(!v.accepted());
        assert_eq!(v.output, ORIGINAL);
        assert!(matches!(
            v.record.fallback,
            Some(FallbackReason::InvariantLost(_))
        ));
    }

    #[test]
    fn rejects_broken_structure() {
        let c = cfg(Mode::Safe);
        let candidate = Candidate::new("test.demo", 1, "not ir at all".to_string())
            .reparse(Reparse::TokenIr)
            .invariants(vec![]);
        let v = Firewall::new(&c).review(ORIGINAL, candidate);
        assert!(!v.accepted());
        assert!(matches!(
            v.record.fallback,
            Some(FallbackReason::ReparseFailed(_)) | Some(FallbackReason::InvariantLost(_))
        ));
    }

    #[test]
    fn rejects_a_transformation_without_gain() {
        let c = cfg(Mode::Safe);
        let mut bloated = ORIGINAL.to_string();
        bloated.push_str("and now some extra text that makes it longer than before\n");
        let candidate = Candidate::new("noop", 1, bloated);
        let v = Firewall::new(&c).review(ORIGINAL, candidate);
        assert!(!v.accepted());
        assert!(matches!(v.record.fallback, Some(FallbackReason::NoGain)));
    }

    #[test]
    fn observe_mode_never_changes_bytes() {
        let c = cfg(Mode::Observe);
        let v = Firewall::new(&c).review(ORIGINAL, good_candidate());
        assert_eq!(v.output, ORIGINAL);
        assert_eq!(v.record.validation, ValidationOutcome::Skipped);
        assert!(v.record.notes[0].contains("would have saved"));
    }

    #[test]
    fn lossy_needs_balanced_or_maximum() {
        let candidate = good_candidate().lossy(true);
        let safe = cfg(Mode::Safe);
        assert!(
            !Firewall::new(&safe)
                .review(ORIGINAL, candidate.clone())
                .accepted()
        );

        let balanced = cfg(Mode::Balanced);
        let v = Firewall::new(&balanced).review(ORIGINAL, candidate);
        assert!(v.accepted(), "{:?}", v.record.fallback);
        assert_eq!(v.record.validation, ValidationOutcome::PassedWithWarnings);
    }

    #[test]
    fn json_equivalence_is_enforced() {
        let c = cfg(Mode::Safe);
        let original = "{\n    \"a\" : [\n        1 ,\n        2 ,\n        3\n    ] ,\n    \"b\" : {\n        \"c\" : \"d\"\n    }\n}\n";
        let ok = Candidate::new("json.minify", 1, r#"{"a":[1,2,3],"b":{"c":"d"}}"#.into())
            .reparse(Reparse::JsonEquivalent);
        assert!(Firewall::new(&c).review(original, ok).accepted());

        let bad = Candidate::new("json.lossy", 1, r#"{"a":[1,2],"b":{}}"#.into())
            .reparse(Reparse::JsonEquivalent);
        let v = Firewall::new(&c).review(original, bad);
        assert!(!v.accepted());
    }

    #[test]
    fn a_panicking_compiler_falls_back() {
        let v = guarded("boom", 1, ORIGINAL, || panic!("compiler exploded"))
            .expect_err("must be a fallback verdict");
        assert_eq!(v.output, ORIGINAL);
        assert!(matches!(
            v.record.fallback,
            Some(FallbackReason::TransformerError(ref m)) if m.contains("compiler exploded")
        ));
    }

    #[test]
    fn guarded_passes_through_success() {
        let c =
            guarded("ok", 1, ORIGINAL, || Candidate::new("ok", 1, "x".into())).expect("no panic");
        assert_eq!(c.output, "x");
    }
}
