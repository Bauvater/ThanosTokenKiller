//! The vertical pipeline.
//!
//! ```text
//! content
//!   → content detection
//!   → secret redaction          (local mapping, never leaves the machine)
//!   → capsule store             (byte exact original, always)
//!   → learned filter            (rules an agent taught with <filter-trash>)
//!   → quality firewall          (invariants, re-parse, gain)
//!   → candidates:               repeat · delta · specialized compiler
//!   → quality firewall          (reviews each; the smallest survivor wins)
//!   → token event + JSONL log
//!   → final output
//! ```
//!
//! Every stage can decline; declining always means "hand through the original
//! unchanged". There is no path through this function that can lose data
//! without a capsule holding it.

use serde_json::json;

use crate::delta;
use ttk_compilers::{CommandContext, CompileInput};
use ttk_core::config::Config;
use ttk_core::content::{self, Detection};
use ttk_core::event::{EventDirection, EventSource, TokenEvent};
use ttk_core::firewall::{Candidate, Firewall, Verdict};
use ttk_core::ids::SessionId;
use ttk_core::redact;
use ttk_core::trust::{SensitivityLevel, TrustLevel};
use ttk_core::{Result, tokens};
use ttk_learn::{Engine, Filtered, RuleSet};
use ttk_store::{NewCapsule, Workspace};

pub struct PipelineInput<'a> {
    pub content: &'a str,
    pub source: EventSource,
    pub direction: EventDirection,
    pub trust: TrustLevel,
    pub command: Option<&'a CommandContext>,
    /// Name hint for content detection, e.g. a file name.
    pub name: Option<&'a str>,
    pub session: SessionId,
    /// Learned filter rules to apply. `None` disables the stage entirely,
    /// which is what `--no-filter` and a dry run use.
    pub rules: Option<&'a RuleSet>,
}

impl<'a> PipelineInput<'a> {
    pub fn new(content: &'a str, session: SessionId) -> Self {
        Self {
            content,
            source: EventSource::ShellOutput,
            direction: EventDirection::Inbound,
            trust: TrustLevel::RepositoryUntrusted,
            command: None,
            name: None,
            session,
            rules: None,
        }
    }

    pub fn rules(mut self, rules: &'a RuleSet) -> Self {
        self.rules = Some(rules);
        self
    }

    pub fn command(mut self, c: &'a CommandContext) -> Self {
        self.command = Some(c);
        self.source = EventSource::ShellOutput;
        self
    }

    pub fn name(mut self, n: &'a str) -> Self {
        self.name = Some(n);
        self
    }
}

pub struct PipelineOutput {
    /// What should be shown to the model / the user.
    pub content: String,
    pub event: TokenEvent,
    pub detection: Detection,
    /// `cap://<id>` of the stored original, if one was written.
    pub capsule_ref: Option<String>,
    pub secrets_redacted: usize,
    /// What the learned filter did, when it ran and was accepted.
    pub filtered: Option<Filtered>,
    /// Which transformer produced the final content, if any did.
    pub winner: Option<String>,
    /// Rules matched a line the error guard refused to remove. A number above
    /// zero means a rule is drifting and is worth reporting.
    pub protected_lines: u64,
}

/// Union of two invariant lists, deduplicated and sorted.
///
/// Mirrors what `TokenEvent::apply` does, needed here because the winning
/// candidate is applied by hand rather than through `apply`: several candidates
/// are reviewed and only one is kept.
fn merge_invariants(
    a: &[ttk_core::invariants::Invariant],
    b: &[ttk_core::invariants::Invariant],
) -> Vec<ttk_core::invariants::Invariant> {
    let mut out = a.to_vec();
    for inv in b {
        if !out.contains(inv) {
            out.push(inv.clone());
        }
    }
    out.sort();
    out
}

/// Run the full pipeline for one piece of content.
pub fn process(
    workspace: &Workspace,
    config: &Config,
    input: PipelineInput<'_>,
) -> Result<PipelineOutput> {
    let detection = content::detect(input.content, input.name);

    // 1. Redaction. Everything downstream sees the redacted text; only the
    //    capsule (local) knows the real values.
    let redacted = if config.security.redact_secrets {
        redact::redact(input.content, config.security.redact_pii)
    } else {
        redact::Redacted {
            text: input.content.to_string(),
            ..Default::default()
        }
    };
    let sensitivity = if redacted.is_clean() {
        SensitivityLevel::Internal
    } else {
        SensitivityLevel::Secret
    };

    let mut event = TokenEvent::capture(
        input.session.clone(),
        input.source,
        input.direction,
        detection.content_type,
        &redacted.text,
    )
    .with_trust(input.trust)
    .with_sensitivity(sensitivity)
    .with_metadata("detection", json!(detection.reason))
    .with_metadata("mode", json!(config.mode.as_str()));

    if !redacted.hits.is_empty() {
        event = event.with_metadata("secrets_redacted", json!(redacted.hits.len()));
    }

    if let Some(cmd) = input.command {
        event = event
            .with_metadata("command", json!(cmd.command_line()))
            .with_metadata("exit_code", json!(cmd.exit_code))
            .with_metadata("duration_ms", json!(cmd.duration_ms));
    }

    // 2. Store the original. Always — even in observe mode, because that is
    //    what makes every later removal reversible.
    let capsule = workspace.capsules().put(
        NewCapsule::new(input.content, detection.content_type)
            .event(event.id.clone())
            .trust(input.trust)
            .sensitivity(sensitivity)
            .secret_mapping(redacted.mapping.clone())
            .ttl_days(config.capsules.raw_ttl_days)
            .meta("source", json!(input.source.as_str()))
            .meta("command", json!(input.command.map(|c| c.command_line()))),
    )?;
    // The short handle, not the full id: this string is printed on every
    // compiled output, and a 26 character random id costs more tokens than
    // some whole compiled messages.
    let capsule_ref = capsule.reference();
    event.capsule_id = Some(capsule.id.clone());
    event.original_content_ref.stored = true;

    // 3. Learned filter. Before the compiler on purpose: a compiler should
    //    not have to parse around noise somebody already retired, and the
    //    saving compounds instead of competing.
    let mut working = redacted.text.clone();
    let mut filtered = None;
    let mut protected_lines = 0;
    let mut winner: Option<String> = None;
    if config.learning.enabled
        && let Some(rules) = input.rules
    {
        let program = input.command.and_then(CommandContext::program);
        let subcommand = input.command.and_then(CommandContext::subcommand);
        let engine = Engine::for_command(rules, program.as_deref(), subcommand)
            .protect_errors(config.learning.protect_errors);
        if !engine.is_empty() {
            let pass = engine.filter(&working);
            protected_lines = pass.protected_lines;
            if pass.changed() {
                // A learned filter is an explicit instruction, so it does not
                // have to clear the compilers' relative-gain bar — but every
                // other firewall check still applies to it unchanged.
                let candidate = Candidate::new("learn.filter", 1, pass.text.clone())
                    .min_gain(0.0)
                    .note(pass.summary());
                let verdict = Firewall::new(config).review(&working, candidate);
                let accepted = verdict.accepted();
                event.apply(verdict.output.clone(), verdict.record);
                if accepted {
                    working = verdict.output;
                    filtered = Some(pass);
                } else {
                    // `apply` recorded the attempt; the content stays original.
                    event.transformed_content = None;
                    event.estimated_tokens_after = event.estimated_tokens_before;
                }
            }
        }
        if protected_lines > 0 {
            event
                .metadata
                .insert("filter_protected_lines".to_string(), json!(protected_lines));
        }
    }

    // 4. Candidates. A compiler is one of several ways to say the same thing
    //    in fewer tokens, and it is not always the best one: eleven identical
    //    `cargo test` runs in a row are better answered by "the same as two
    //    minutes ago" than by eleven identical compilations of the same
    //    passing summary.
    //
    //    Every candidate goes through the same firewall, and the smallest one
    //    that survives wins. None of them is trusted to judge itself, and
    //    every one of them carries the error-shaped lines forward — which is
    //    why the firewall can accept a pointer at all.
    let command_line = input.command.map(CommandContext::command_line);
    let previous = delta::previous_run(workspace, command_line.as_deref(), event.timestamp_millis);

    let mut candidates: Vec<Candidate> = Vec::new();
    if let Some(prev) = &previous {
        if let Some(c) = delta::repeat(&working, &event.source_hash, prev, event.timestamp_millis) {
            candidates.push(c);
        } else if let Some(c) = delta::delta(&working, prev, event.timestamp_millis, config) {
            candidates.push(c);
        }
    }

    let mut compile_input = CompileInput::new(&working, config)
        .source(input.source)
        .content_type(detection.content_type)
        .capsule(&capsule_ref);
    if let Some(c) = input.command {
        compile_input = compile_input.command(c);
    }

    if let Some(candidate) = ttk_compilers::compile(&compile_input) {
        candidates.push(candidate);
    } else if candidates.is_empty() {
        event
            .metadata
            .insert("no_compiler".to_string(), json!(true));
    }

    // 5. Firewall. It owns the decision, not the candidate.
    let firewall = Firewall::new(config);
    let mut best: Option<Verdict> = None;
    for candidate in candidates {
        let verdict = firewall.review(&working, candidate);
        let accepted = verdict.accepted();
        // Every attempt is recorded, accepted or not: a rejected candidate is
        // exactly what `ttk explain` has to be able to show.
        event.transformations.push(verdict.record.clone());
        if !accepted {
            continue;
        }
        let better = best
            .as_ref()
            .is_none_or(|b| verdict.output.len() < b.output.len());
        if better {
            best = Some(verdict);
        }
    }

    let final_content = match best {
        Some(verdict) => {
            event.invariants = merge_invariants(&event.invariants, &verdict.record.invariants);
            event.estimated_tokens_after = verdict.record.tokens_after;
            event.transformed_content = Some(verdict.output.clone());
            event
                .metadata
                .insert("winner".to_string(), json!(verdict.record.transformer));
            winner = Some(verdict.record.transformer.clone());
            verdict.output
        }
        None => {
            // Nothing beat the input, so the filtered content stands.
            event.transformed_content = (working != redacted.text).then(|| working.clone());
            event.estimated_tokens_after = tokens::estimate(&working);
            working.clone()
        }
    };

    event.estimated_tokens_after = tokens::estimate(&final_content);
    workspace.events().append(&event)?;

    Ok(PipelineOutput {
        content: final_content,
        event,
        detection,
        capsule_ref: Some(capsule_ref),
        secrets_redacted: redacted.hits.len(),
        filtered,
        protected_lines,
        winner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttk_core::config::Mode;

    struct Fx {
        _dir: tempfile::TempDir,
        ws: Workspace,
    }

    fn fixture(config: &Config) -> Fx {
        let dir = tempfile::tempdir().expect("tmp");
        let ws = Workspace::open_at(&dir.path().join(".ttk"), config).expect("workspace");
        Fx { _dir: dir, ws }
    }

    fn cfg(mode: Mode) -> Config {
        Config {
            mode,
            ..Config::default()
        }
    }

    const PYTEST: &str = "============================= test session starts ====\n\
        collected 3 items\n\
        tests/a.py ..F\n\
        =================================== FAILURES ===================================\n\
        ______________________________ test_x _______________________________\n\
        E       assert 200 == 401\n\
        \n\
        tests/a.py:87: AssertionError\n\
        =========================== short test summary info ============================\n\
        FAILED tests/a.py::test_x - assert 200 == 401\n\
        ======================== 1 failed, 2 passed in 1.20s =============\n";

    #[test]
    fn compresses_and_keeps_the_original_retrievable() {
        let config = cfg(Mode::Safe);
        let fx = fixture(&config);
        let out = process(
            &fx.ws,
            &config,
            PipelineInput::new(PYTEST, SessionId::new()),
        )
        .expect("pipeline");

        assert!(out.event.was_transformed());
        assert!(out.event.tokens_saved().value > 0);
        assert!(out.content.starts_with("[test:pytest]"));
        assert!(out.content.contains("tests/a.py:87"));

        let cap = out.capsule_ref.as_ref().expect("capsule");
        let raw = fx
            .ws
            .capsules()
            .render(cap, ttk_store::LEVEL_RAW, false)
            .expect("raw");
        assert_eq!(raw, PYTEST, "the original must round trip byte exactly");
    }

    #[test]
    fn observe_mode_returns_the_input_unchanged() {
        let config = cfg(Mode::Observe);
        let fx = fixture(&config);
        let out = process(
            &fx.ws,
            &config,
            PipelineInput::new(PYTEST, SessionId::new()),
        )
        .expect("pipeline");
        assert_eq!(out.content, PYTEST);
        assert!(!out.event.was_transformed());
        // The potential saving is still recorded.
        assert!(!out.event.transformations.is_empty());
    }

    #[test]
    fn secrets_are_redacted_before_anything_else_sees_them() {
        let config = cfg(Mode::Safe);
        let fx = fixture(&config);
        let content = format!("{PYTEST}api_key = supersecretvalue123\n");
        let out = process(
            &fx.ws,
            &config,
            PipelineInput::new(&content, SessionId::new()),
        )
        .expect("pipeline");

        assert_eq!(out.secrets_redacted, 1);
        assert!(!out.content.contains("supersecretvalue123"));
        assert_eq!(
            out.event.sensitivity,
            ttk_core::trust::SensitivityLevel::Secret
        );

        let cap = out.capsule_ref.as_ref().expect("capsule");
        // Raw access is gated…
        assert!(fx.ws.capsules().raw(cap, false).is_err());
        // …but the value is still recoverable locally.
        let raw = fx
            .ws
            .capsules()
            .render(cap, ttk_store::LEVEL_RAW, true)
            .expect("raw");
        assert!(raw.contains("supersecretvalue123"));
    }

    #[test]
    fn unknown_content_passes_through_untouched() {
        let config = cfg(Mode::Maximum);
        let fx = fixture(&config);
        let content = "a single unremarkable line";
        let out = process(
            &fx.ws,
            &config,
            PipelineInput::new(content, SessionId::new()),
        )
        .expect("pipeline");
        assert_eq!(out.content, content);
        assert!(!out.event.was_transformed());
        assert_eq!(out.event.metadata.get("no_compiler"), Some(&json!(true)));
    }

    #[test]
    fn every_run_is_logged() {
        let config = cfg(Mode::Safe);
        let fx = fixture(&config);
        let session = SessionId::new();
        for _ in 0..3 {
            process(&fx.ws, &config, PipelineInput::new(PYTEST, session.clone())).expect("run");
        }
        let events = fx.ws.events().read_session(&session).expect("events");
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.capsule_id.is_some()));
    }
}
