//! Honest reporting.
//!
//! Rules that this module enforces mechanically:
//!
//! * every number carries its measurement method (`~` marks an estimate),
//! * transformations that were rejected are reported as rejected, with the
//!   reason, and contribute **zero** saving,
//! * a total saving is never larger than the sum of its parts,
//! * nothing is extrapolated: what is not measured is not shown.

use std::collections::BTreeMap;

use serde::Serialize;
use ttk_core::event::{TokenEvent, ValidationOutcome};
use ttk_core::tokens::{CountMethod, TokenCount};

#[derive(Debug, Clone, Default, Serialize)]
pub struct TransformerStat {
    pub transformer: String,
    pub accepted: u64,
    pub rejected: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// Reasons for rejection, counted.
    pub fallbacks: BTreeMap<String, u64>,
}

impl TransformerStat {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GainReport {
    pub session: String,
    pub events: u64,
    pub events_transformed: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// The weakest measurement method involved, so the total is never
    /// presented as more precise than its inputs.
    pub method: String,
    pub capsules_written: u64,
    pub secrets_redacted: u64,
    pub by_transformer: Vec<TransformerStat>,
}

impl GainReport {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn saved_percent(&self) -> f64 {
        if self.tokens_before == 0 {
            return 0.0;
        }
        self.saved() as f64 * 100.0 / self.tokens_before as f64
    }

    pub fn build(session: &str, events: &[TokenEvent]) -> Self {
        let mut report = GainReport {
            session: session.to_string(),
            events: events.len() as u64,
            method: CountMethod::Tokenizer.as_str().to_string(),
            ..Default::default()
        };
        let mut stats: BTreeMap<String, TransformerStat> = BTreeMap::new();
        let mut weakest = CountMethod::Provider;

        for event in events {
            let before = event.tokens_before();
            let after = event.tokens_after();
            report.tokens_before += before.value;
            report.tokens_after += after.value;
            weakest = weaker(weakest, before.method);
            weakest = weaker(weakest, after.method);

            if event.was_transformed() {
                report.events_transformed += 1;
            }
            if event.capsule_id.is_some() {
                report.capsules_written += 1;
            }
            report.secrets_redacted += event
                .metadata
                .get("secrets_redacted")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            for t in &event.transformations {
                let entry = stats.entry(t.transformer.clone()).or_default();
                entry.transformer = t.transformer.clone();
                match t.validation {
                    ValidationOutcome::Passed | ValidationOutcome::PassedWithWarnings => {
                        entry.accepted += 1;
                        entry.tokens_before += t.tokens_before.value;
                        entry.tokens_after += t.tokens_after.value;
                    }
                    ValidationOutcome::Failed | ValidationOutcome::Skipped => {
                        entry.rejected += 1;
                        // A rejected transformation saves nothing, by
                        // definition: the original was kept.
                        entry.tokens_before += t.tokens_before.value;
                        entry.tokens_after += t.tokens_before.value;
                        if let Some(f) = &t.fallback {
                            *entry.fallbacks.entry(f.summary()).or_default() += 1;
                        }
                    }
                }
            }
        }

        report.method = weakest.as_str().to_string();
        report.by_transformer = stats.into_values().collect();
        report
            .by_transformer
            .sort_by_key(|s| std::cmp::Reverse(s.saved()));
        report
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let est = self.method == CountMethod::Estimated.as_str();
        let m = |v: u64| if est { format!("~{v}") } else { v.to_string() };

        out.push_str(&format!("session          {}\n", self.session));
        out.push_str(&format!(
            "events           {} ({} transformed)\n",
            self.events, self.events_transformed
        ));
        out.push_str(&format!("tokens before    {:>12}\n", m(self.tokens_before)));
        out.push_str(&format!("tokens after     {:>12}\n", m(self.tokens_after)));
        out.push_str(&format!(
            "saved            {:>12}  ({:.1}%)\n",
            m(self.saved()),
            self.saved_percent()
        ));
        out.push_str(&format!("counting method  {}\n", self.method));
        out.push_str(&format!("capsules written {}\n", self.capsules_written));
        if self.secrets_redacted > 0 {
            out.push_str(&format!("secrets redacted {}\n", self.secrets_redacted));
        }

        if !self.by_transformer.is_empty() {
            out.push_str("\nby transformer\n");
            for s in &self.by_transformer {
                out.push_str(&format!(
                    "  {:<18} {:>10}  accepted={} rejected={}\n",
                    s.transformer,
                    format!("-{}", m(s.saved())),
                    s.accepted,
                    s.rejected
                ));
                for (reason, n) in &s.fallbacks {
                    out.push_str(&format!("      fallback x{n}: {reason}\n"));
                }
            }
        }

        if est {
            out.push_str(
                "\nnote: `~` marks heuristic estimates. Exact counts require a provider\n\
                 usage field or a model tokenizer; neither was available for this session.\n",
            );
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Lifetime statistics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct CommandStat {
    /// Program name, e.g. `cargo`, `git`, `pytest`.
    pub program: String,
    pub runs: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
}

impl CommandStat {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }
}

/// Everything ThanosTokenKiller has done in this workspace, across all
/// sessions. Built from the same events `ttk gain` uses, so the two can never
/// disagree.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StatsReport {
    pub sessions: u64,
    pub events: u64,
    pub events_transformed: u64,
    /// Events that came from a captured command (`ttk run`).
    pub commands_run: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub method: String,
    pub transformations_accepted: u64,
    pub transformations_rejected: u64,
    pub secrets_redacted: u64,
    /// Unix millis of the first and last recorded event.
    pub first_event_millis: Option<u64>,
    pub last_event_millis: Option<u64>,
    pub by_transformer: Vec<TransformerStat>,
    pub by_command: Vec<CommandStat>,
    // Storage side, from the capsule store rather than the event log.
    pub capsules: u64,
    pub blob_bytes: u64,
    pub original_bytes: u64,
}

impl StatsReport {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn saved_percent(&self) -> f64 {
        if self.tokens_before == 0 {
            return 0.0;
        }
        self.saved() as f64 * 100.0 / self.tokens_before as f64
    }

    /// Accumulate one session's events.
    pub fn add_session(&mut self, events: &[TokenEvent]) {
        if events.is_empty() {
            return;
        }
        self.sessions += 1;
        let mut transformers: BTreeMap<String, TransformerStat> = self
            .by_transformer
            .drain(..)
            .map(|s| (s.transformer.clone(), s))
            .collect();
        let mut commands: BTreeMap<String, CommandStat> = self
            .by_command
            .drain(..)
            .map(|s| (s.program.clone(), s))
            .collect();
        let mut weakest = if self.method.is_empty() {
            CountMethod::Provider
        } else {
            method_from_str(&self.method)
        };

        for event in events {
            let before = event.tokens_before();
            let after = event.tokens_after();
            self.events += 1;
            self.tokens_before += before.value;
            self.tokens_after += after.value;
            weakest = weaker(weakest, before.method);
            weakest = weaker(weakest, after.method);

            if event.was_transformed() {
                self.events_transformed += 1;
            }
            self.first_event_millis = Some(
                self.first_event_millis
                    .map_or(event.timestamp_millis, |t| t.min(event.timestamp_millis)),
            );
            self.last_event_millis = Some(
                self.last_event_millis
                    .map_or(event.timestamp_millis, |t| t.max(event.timestamp_millis)),
            );

            self.secrets_redacted += event
                .metadata
                .get("secrets_redacted")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if let Some(cmd) = event.metadata.get("command").and_then(|v| v.as_str()) {
                self.commands_run += 1;
                let program = program_of(cmd);
                let entry = commands.entry(program.clone()).or_default();
                entry.program = program;
                entry.runs += 1;
                entry.tokens_before += before.value;
                entry.tokens_after += after.value;
            }

            for t in &event.transformations {
                let entry = transformers.entry(t.transformer.clone()).or_default();
                entry.transformer = t.transformer.clone();
                match t.validation {
                    ValidationOutcome::Passed | ValidationOutcome::PassedWithWarnings => {
                        self.transformations_accepted += 1;
                        entry.accepted += 1;
                        entry.tokens_before += t.tokens_before.value;
                        entry.tokens_after += t.tokens_after.value;
                    }
                    ValidationOutcome::Failed | ValidationOutcome::Skipped => {
                        self.transformations_rejected += 1;
                        entry.rejected += 1;
                        // A rejected step kept the original, so it saved nothing.
                        entry.tokens_before += t.tokens_before.value;
                        entry.tokens_after += t.tokens_before.value;
                        if let Some(f) = &t.fallback {
                            *entry.fallbacks.entry(f.summary()).or_default() += 1;
                        }
                    }
                }
            }
        }

        self.method = weakest.as_str().to_string();
        self.by_transformer = transformers.into_values().collect();
        self.by_transformer
            .sort_by_key(|s| std::cmp::Reverse(s.saved()));
        self.by_command = commands.into_values().collect();
        self.by_command
            .sort_by_key(|s| std::cmp::Reverse(s.saved()));
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let est = self.method == CountMethod::Estimated.as_str();
        let m = |v: u64| if est { format!("~{v}") } else { v.to_string() };

        out.push_str("ThanosTokenKiller — lifetime statistics for this workspace\n\n");
        out.push_str(&format!(
            "tokens saved     {:>14}  ({:.1}% of {})\n",
            m(self.saved()),
            self.saved_percent(),
            m(self.tokens_before)
        ));
        out.push_str(&format!("context sent     {:>14}\n", m(self.tokens_after)));
        out.push_str(&format!("counting method  {:>14}\n", self.method));
        out.push_str(&format!("commands run     {:>14}\n", self.commands_run));
        out.push_str(&format!(
            "events           {:>14}  ({} compiled, {} passed through)\n",
            self.events,
            self.events_transformed,
            self.events.saturating_sub(self.events_transformed)
        ));
        out.push_str(&format!("sessions         {:>14}\n", self.sessions));
        out.push_str(&format!(
            "transformations  {:>14}  ({} accepted, {} fell back)\n",
            self.transformations_accepted + self.transformations_rejected,
            self.transformations_accepted,
            self.transformations_rejected
        ));
        if let (Some(first), Some(last)) = (self.first_event_millis, self.last_event_millis) {
            out.push_str(&format!(
                "active period    {:>14}\n",
                human_span(last.saturating_sub(first))
            ));
        }
        out.push_str(&format!(
            "capsules         {:>14}  ({:.2} MiB on disk for {:.2} MiB of originals)\n",
            self.capsules,
            self.blob_bytes as f64 / 1_048_576.0,
            self.original_bytes as f64 / 1_048_576.0
        ));
        if self.secrets_redacted > 0 {
            out.push_str(&format!("secrets redacted {:>14}\n", self.secrets_redacted));
        }

        if !self.by_command.is_empty() {
            out.push_str("\ntop commands\n");
            for c in self.by_command.iter().take(10) {
                out.push_str(&format!(
                    "  {:<16} {:>12} saved  ({} run{})\n",
                    c.program,
                    m(c.saved()),
                    c.runs,
                    if c.runs == 1 { "" } else { "s" }
                ));
            }
        }

        if !self.by_transformer.is_empty() {
            out.push_str("\nby transformer\n");
            for s in self.by_transformer.iter().take(15) {
                out.push_str(&format!(
                    "  {:<18} {:>12} saved  accepted={} fell_back={}\n",
                    s.transformer,
                    m(s.saved()),
                    s.accepted,
                    s.rejected
                ));
            }
        }

        if self.events == 0 {
            out.push_str("\nnothing recorded yet — try `ttk run -- cargo test`\n");
        } else if est {
            out.push_str(
                "\nnote: `~` marks heuristic estimates. Exact counts need a provider usage\n\
                 field or a model tokenizer; neither was available for these events.\n",
            );
        }
        out
    }
}

fn method_from_str(s: &str) -> CountMethod {
    match s {
        "provider" => CountMethod::Provider,
        "tokenizer" => CountMethod::Tokenizer,
        "estimated" => CountMethod::Estimated,
        _ => CountMethod::Unavailable,
    }
}

/// `"cargo test --quiet"` → `cargo`; a path or `.exe` suffix is stripped.
fn program_of(command_line: &str) -> String {
    // The program itself may be a quoted path containing spaces, so a plain
    // whitespace split is not enough.
    let first = match command_line.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command_line
            .split_whitespace()
            .next()
            .unwrap_or(command_line),
    };
    let base = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if base.is_empty() {
        "(unknown)".to_string()
    } else {
        base.to_string()
    }
}

fn human_span(millis: u64) -> String {
    let secs = millis / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn weaker(a: CountMethod, b: CountMethod) -> CountMethod {
    fn rank(m: CountMethod) -> u8 {
        match m {
            CountMethod::Provider => 3,
            CountMethod::Tokenizer => 2,
            CountMethod::Estimated => 1,
            CountMethod::Unavailable => 0,
        }
    }
    if rank(a) <= rank(b) { a } else { b }
}

/// One-line summary printed after `ttk run`.
pub fn one_line(before: TokenCount, after: TokenCount, capsule: Option<&str>) -> String {
    let saved = before.saved_against(after);
    let pct = if before.value == 0 {
        0.0
    } else {
        saved.value as f64 * 100.0 / before.value as f64
    };
    match capsule {
        Some(c) => format!("ttk: {before} → {after} tokens (-{pct:.0}%), full output: {c}"),
        None => format!("ttk: {before} → {after} tokens (-{pct:.0}%)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttk_core::content::ContentType;
    use ttk_core::event::{
        EventDirection, EventSource, FallbackReason, TokenEvent, TransformationRecord,
    };
    use ttk_core::ids::SessionId;

    fn event_with(record: TransformationRecord, transformed: bool) -> TokenEvent {
        let mut e = TokenEvent::capture(
            SessionId::new(),
            EventSource::ShellOutput,
            EventDirection::Inbound,
            ContentType::ShellOutput,
            "a".repeat(400).as_str(),
        );
        e.capsule_id = Some(ttk_core::ids::CapsuleId::new());
        // Keep the event level accounting consistent with the record.
        e.estimated_tokens_before = record.tokens_before;
        if transformed {
            e.apply("short".to_string(), record);
        } else {
            // A rejected step keeps the original, so `after` equals `before` —
            // exactly what the pipeline does.
            e.estimated_tokens_after = record.tokens_before;
            e.transformations.push(record);
        }
        e
    }

    fn record(transformer: &str, before: u64, after: u64, ok: bool) -> TransformationRecord {
        TransformationRecord {
            transformer: transformer.to_string(),
            version: 1,
            at_millis: 0,
            input_hash: "blake3:a".into(),
            output_hash: "blake3:b".into(),
            tokens_before: TokenCount::estimated(before),
            tokens_after: TokenCount::estimated(after),
            invariants: vec![],
            validation: if ok {
                ValidationOutcome::Passed
            } else {
                ValidationOutcome::Failed
            },
            fallback: if ok {
                None
            } else {
                Some(FallbackReason::NoGain)
            },
            notes: vec![],
        }
    }

    #[test]
    fn accepted_transformations_are_credited() {
        let events = vec![event_with(record("test.pytest", 1000, 100, true), true)];
        let r = GainReport::build("se_x", &events);
        assert_eq!(r.events, 1);
        assert_eq!(r.events_transformed, 1);
        assert_eq!(r.by_transformer.len(), 1);
        assert_eq!(r.by_transformer[0].saved(), 900);
        assert_eq!(r.capsules_written, 1);
    }

    #[test]
    fn rejected_transformations_save_nothing() {
        let events = vec![event_with(record("log.cluster", 1000, 100, false), false)];
        let r = GainReport::build("se_x", &events);
        assert_eq!(
            r.by_transformer[0].saved(),
            0,
            "a rejected step cannot claim a saving"
        );
        assert_eq!(r.by_transformer[0].rejected, 1);
        assert!(r.render().contains("fallback x1"));
    }

    #[test]
    fn estimates_are_marked_in_the_render() {
        let events = vec![event_with(record("test.pytest", 1000, 100, true), true)];
        let text = GainReport::build("se_x", &events).render();
        assert!(text.contains('~'), "{text}");
        assert!(text.contains("heuristic estimates"));
    }

    #[test]
    fn empty_session_reports_zero() {
        let r = GainReport::build("se_x", &[]);
        assert_eq!(r.saved(), 0);
        assert_eq!(r.saved_percent(), 0.0);
        assert!(r.render().contains("tokens before"));
    }

    #[test]
    fn stats_accumulate_across_sessions() {
        let mut report = StatsReport::default();
        for _ in 0..3 {
            let mut e = event_with(record("test.pytest", 1000, 100, true), true);
            e.metadata
                .insert("command".into(), serde_json::json!("cargo test --quiet"));
            report.add_session(&[e]);
        }
        assert_eq!(report.sessions, 3);
        assert_eq!(report.events, 3);
        assert_eq!(report.commands_run, 3);
        assert_eq!(report.transformations_accepted, 3);
        assert_eq!(report.by_command.len(), 1);
        assert_eq!(report.by_command[0].program, "cargo");
        assert_eq!(report.by_command[0].runs, 3);
        assert_eq!(report.by_transformer[0].saved(), 2700);
        assert!(report.saved() > 0);
        assert_eq!(report.method, "estimated");
    }

    #[test]
    fn stats_do_not_credit_rejected_steps() {
        let mut report = StatsReport::default();
        report.add_session(&[event_with(record("log.cluster", 1000, 100, false), false)]);
        assert_eq!(report.transformations_rejected, 1);
        assert_eq!(report.by_transformer[0].saved(), 0);
        assert_eq!(report.saved(), 0);
    }

    #[test]
    fn empty_stats_render_a_hint_instead_of_zeroes() {
        let text = StatsReport::default().render();
        assert!(text.contains("nothing recorded yet"), "{text}");
        assert_eq!(StatsReport::default().saved_percent(), 0.0);
    }

    #[test]
    fn program_names_are_normalised() {
        assert_eq!(
            program_of(r#""C:\Program Files\Git\cmd\GIT.EXE" status"#),
            "git"
        );
        assert_eq!(program_of("/usr/bin/cargo test"), "cargo");
        assert_eq!(program_of(""), "(unknown)");
    }

    #[test]
    fn spans_are_human_readable() {
        assert_eq!(human_span(5_000), "5s");
        assert_eq!(human_span(120_000), "2m");
        assert_eq!(human_span(7_200_000), "2h");
        assert_eq!(human_span(172_800_000), "2d");
    }

    #[test]
    fn one_line_summary() {
        let s = one_line(
            TokenCount::estimated(1000),
            TokenCount::estimated(250),
            Some("cap://cap_1"),
        );
        assert!(s.contains("-75%"), "{s}");
        assert!(s.contains("cap://cap_1"));
        assert!(s.contains('~'));
    }
}
