//! The vertical pipeline.
//!
//! ```text
//! content
//!   → content detection
//!   → secret redaction          (local mapping, never leaves the machine)
//!   → capsule store             (byte exact original, always)
//!   → specialized compiler      (Token IR)
//!   → quality firewall          (invariants, re-parse, gain)
//!   → token event + JSONL log
//!   → final output
//! ```
//!
//! Every stage can decline; declining always means "hand through the original
//! unchanged". There is no path through this function that can lose data
//! without a capsule holding it.

use serde_json::json;

use ttk_compilers::{CommandContext, CompileInput};
use ttk_core::config::Config;
use ttk_core::content::{self, Detection};
use ttk_core::event::{EventDirection, EventSource, TokenEvent};
use ttk_core::firewall::Firewall;
use ttk_core::ids::SessionId;
use ttk_core::redact;
use ttk_core::trust::{SensitivityLevel, TrustLevel};
use ttk_core::{Result, tokens};
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
        }
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
    let capsule_ref = format!("cap://{}", capsule.id);
    event.capsule_id = Some(capsule.id.clone());
    event.original_content_ref.stored = true;

    // 3. Compile.
    let mut compile_input = CompileInput::new(&redacted.text, config)
        .source(input.source)
        .content_type(detection.content_type)
        .capsule(&capsule_ref);
    if let Some(c) = input.command {
        compile_input = compile_input.command(c);
    }

    let final_content = match ttk_compilers::compile(&compile_input) {
        Some(candidate) => {
            // 4. Firewall. It owns the decision, not the compiler.
            let verdict = Firewall::new(config).review(&redacted.text, candidate);
            let accepted = verdict.accepted();
            let output = verdict.output.clone();
            event.apply(output.clone(), verdict.record);
            if !accepted {
                // `apply` recorded the attempt; the content stays original.
                event.transformed_content = None;
                event.estimated_tokens_after = event.estimated_tokens_before;
            }
            output
        }
        None => {
            event
                .metadata
                .insert("no_compiler".to_string(), json!(true));
            redacted.text.clone()
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
