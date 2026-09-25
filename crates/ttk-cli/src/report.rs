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

use crate::ui::{self, Align};

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
        let est = self.method == CountMethod::Estimated.as_str();
        let m = |v: u64| ui::tokens(v, est);
        let mut out = String::new();

        out.push_str(&format!(
            "{}\n",
            ui::banner(&format!("session {}", self.session))
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_note(
                "events",
                self.events,
                format!("{} compiled", self.events_transformed)
            )
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_num(
                "tokens",
                format!(
                    "{} {} {}",
                    m(self.tokens_before),
                    ui::glyphs().arrow,
                    ui::paint(ui::ACCENT, m(self.tokens_after))
                ),
                ""
            )
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_note(
                "saved",
                format!(
                    "{:>12}  {}",
                    ui::paint(ui::OK, m(self.saved())),
                    ui::bar(self.saved_percent() / 100.0, 24)
                ),
                format!("{:.1}%", self.saved_percent())
            )
        ));
        out.push_str(&format!("{}\n", ui::kv("counting method", &self.method)));
        out.push_str(&format!(
            "{}\n",
            ui::kv("capsules written", self.capsules_written)
        ));
        if self.secrets_redacted > 0 {
            out.push_str(&format!(
                "{}\n",
                ui::kv(
                    "secrets redacted",
                    ui::paint(ui::WARN, self.secrets_redacted)
                )
            ));
        }

        if !self.by_transformer.is_empty() {
            out.push_str(&format!("{}\n", ui::heading("by transformer")));
            let rows: Vec<Vec<String>> = self
                .by_transformer
                .iter()
                .map(|s| {
                    vec![
                        s.transformer.clone(),
                        ui::paint(ui::OK, format!("-{}", m(s.saved()))),
                        s.accepted.to_string(),
                        if s.rejected == 0 {
                            "0".to_string()
                        } else {
                            ui::paint(ui::WARN, s.rejected)
                        },
                    ]
                })
                .collect();
            out.push_str(&ui::table(
                &["transformer", "saved", "kept", "fell back"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
                &rows,
            ));
            for s in &self.by_transformer {
                for (reason, n) in &s.fallbacks {
                    out.push_str(&format!(
                        "{}\n",
                        ui::detail(format!("{} fell back x{n}: {reason}", s.transformer))
                    ));
                }
            }
        }

        if est {
            out.push_str(&format!(
                "\n{}\n",
                ui::paint(
                    ui::DIM,
                    "`~` marks heuristic estimates. Exact counts need a provider usage \
                     field or a model tokenizer; neither was available here."
                )
            ));
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
        let est = self.method == CountMethod::Estimated.as_str();
        let m = |v: u64| ui::tokens(v, est);
        let mut out = String::new();

        out.push_str(&format!("{}\n\n", ui::banner("lifetime statistics")));

        if self.events == 0 {
            out.push_str(&format!(
                "{}\n\n",
                ui::paint(ui::DIM, "nothing recorded in this workspace yet.")
            ));
            out.push_str(&format!("{}\n", ui::hint("try ", "ttk run -- cargo test")));
            out.push_str(&format!("{}\n", ui::hint("then", "ttk stats")));
            return out;
        }

        // The headline: one number, one bar, and the share it represents.
        out.push_str(&format!(
            "  {}  {}  {}\n",
            ui::paint(ui::ACCENT, format!("{:>14}", m(self.saved()))),
            ui::bar(self.saved_percent() / 100.0, 28),
            ui::paint(
                ui::DIM,
                format!(
                    "{:.1}% of {} tokens",
                    self.saved_percent(),
                    m(self.tokens_before)
                )
            )
        ));
        out.push_str(&format!(
            "  {}\n\n",
            ui::paint(ui::DIM, "tokens that never reached a model")
        ));

        out.push_str(&format!(
            "{}\n",
            ui::kv_num("context sent", m(self.tokens_after), "")
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_num("commands run", self.commands_run, "")
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_num(
                "events",
                self.events,
                format!(
                    "{} compiled, {} passed through",
                    self.events_transformed,
                    self.events.saturating_sub(self.events_transformed)
                )
            )
        ));
        out.push_str(&format!("{}\n", ui::kv_num("sessions", self.sessions, "")));
        out.push_str(&format!(
            "{}\n",
            ui::kv_num(
                "transformations",
                self.transformations_accepted + self.transformations_rejected,
                format!(
                    "{} accepted, {} fell back",
                    self.transformations_accepted, self.transformations_rejected
                )
            )
        ));
        if let (Some(first), Some(last)) = (self.first_event_millis, self.last_event_millis) {
            out.push_str(&format!(
                "{}\n",
                ui::kv_num("active period", human_span(last.saturating_sub(first)), "")
            ));
        }
        out.push_str(&format!(
            "{}\n",
            ui::kv_num(
                "capsules",
                self.capsules,
                format!(
                    "{:.2} MiB on disk for {:.2} MiB of originals",
                    self.blob_bytes as f64 / 1_048_576.0,
                    self.original_bytes as f64 / 1_048_576.0
                )
            )
        ));
        out.push_str(&format!(
            "{}\n",
            ui::kv_num("counting method", &self.method, "")
        ));
        if self.secrets_redacted > 0 {
            out.push_str(&format!(
                "{}\n",
                ui::kv_num(
                    "secrets redacted",
                    ui::paint(ui::WARN, self.secrets_redacted),
                    "kept local, never sent"
                )
            ));
        }

        if !self.by_command.is_empty() {
            out.push_str(&format!("{}\n", ui::heading("where the savings came from")));
            let top = self
                .by_command
                .first()
                .map(CommandStat::saved)
                .unwrap_or(1)
                .max(1);
            let rows: Vec<Vec<String>> = self
                .by_command
                .iter()
                .take(10)
                .map(|c| {
                    vec![
                        c.program.clone(),
                        ui::paint(ui::OK, m(c.saved())),
                        ui::bar(c.saved() as f64 / top as f64, 16),
                        format!("{} run{}", c.runs, if c.runs == 1 { "" } else { "s" }),
                    ]
                })
                .collect();
            out.push_str(&ui::table(
                &["command", "saved", "", "runs"],
                &[Align::Left, Align::Right, Align::Left, Align::Right],
                &rows,
            ));
        }

        if !self.by_transformer.is_empty() {
            out.push_str(&format!("{}\n", ui::heading("by transformer")));
            let rows: Vec<Vec<String>> = self
                .by_transformer
                .iter()
                .take(15)
                .map(|s| {
                    vec![
                        s.transformer.clone(),
                        ui::paint(ui::OK, m(s.saved())),
                        s.accepted.to_string(),
                        if s.rejected == 0 {
                            "0".to_string()
                        } else {
                            ui::paint(ui::WARN, s.rejected)
                        },
                    ]
                })
                .collect();
            out.push_str(&ui::table(
                &["transformer", "saved", "kept", "fell back"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
                &rows,
            ));
        }

        if est {
            out.push_str(&format!(
                "\n{} {}\n",
                ui::paint(ui::DIM, ui::glyphs().bullet),
                ui::paint(
                    ui::DIM,
                    "`~` marks heuristic estimates: exact counts need a provider usage field."
                )
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The global ledger
// ---------------------------------------------------------------------------

/// Render everything ttk has saved, across every project.
///
/// This is the report people come back for, so it leads with one number and
/// one bar and only then explains itself. The per-project table is what a
/// workspace-local `ttk stats` can never show.
pub fn render_global(usage: &ttk_store::GlobalUsage) -> String {
    let est = usage.method != "provider" && usage.method != "tokenizer";
    let m = |v: u64| ui::tokens(v, est);
    let mut out = String::new();

    out.push_str(&format!(
        "{}\n\n",
        ui::banner("everything ttk has saved, everywhere")
    ));

    if usage.is_empty() {
        out.push_str(&format!(
            "{}\n\n",
            ui::paint(
                ui::DIM,
                "no project has recorded anything yet. Any `ttk run` in any repository\n\
                 lands here, so this fills up on its own."
            )
        ));
        out.push_str(&format!(
            "{}\n",
            ui::hint("start with", "ttk run -- cargo test")
        ));
        if let Some(p) = &usage.path {
            out.push_str(&format!("{}\n", ui::kv("ledger", p.display())));
        }
        return out;
    }

    out.push_str(&format!(
        "  {}  {}  {}\n",
        ui::paint(ui::ACCENT, format!("{:>14}", m(usage.saved()))),
        ui::bar(usage.saved_percent() / 100.0, 28),
        ui::paint(
            ui::DIM,
            format!(
                "{:.1}% of {} tokens",
                usage.saved_percent(),
                m(usage.tokens_before)
            )
        )
    ));
    out.push_str(&format!(
        "  {}\n\n",
        ui::paint(ui::DIM, "tokens that never reached a model")
    ));

    out.push_str(&format!(
        "{}\n",
        ui::kv_num("projects", usage.projects.len(), "")
    ));
    out.push_str(&format!("{}\n", ui::kv_num("commands", usage.events, "")));
    out.push_str(&format!(
        "{}\n",
        ui::kv_num("context sent", m(usage.tokens_after), "")
    ));
    if usage.filtered_lines > 0 {
        out.push_str(&format!(
            "{}\n",
            ui::kv_num(
                "lines filtered",
                ui::thousands(usage.filtered_lines),
                "by rules your agents taught"
            )
        ));
    }
    if let (Some(first), Some(last)) = (usage.first_millis, usage.last_millis) {
        out.push_str(&format!(
            "{}\n",
            ui::kv_num("active period", human_span(last.saturating_sub(first)), "")
        ));
    }
    out.push_str(&format!(
        "{}\n",
        ui::kv_num("counting method", &usage.method, "")
    ));

    // Per project, ranked by what it saved.
    out.push_str(&format!("{}\n", ui::heading("by project")));
    let top = usage
        .projects
        .first()
        .map(|p| p.saved())
        .unwrap_or(1)
        .max(1);
    let rows: Vec<Vec<String>> = usage
        .projects
        .iter()
        .take(15)
        .map(|p| {
            vec![
                ui::ellipsize(&short_path(&p.project), 44),
                ui::paint(ui::OK, m(p.saved())),
                ui::bar(p.saved() as f64 / top as f64, 14),
                format!("{:.0}%", p.saved_percent()),
                ui::thousands(p.events),
            ]
        })
        .collect();
    out.push_str(&ui::table(
        &["project", "saved", "", "rate", "runs"],
        &[
            Align::Left,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
        ],
        &rows,
    ));
    if usage.projects.len() > 15 {
        out.push_str(&format!(
            "{}\n",
            ui::detail(format!("{} more project(s)", usage.projects.len() - 15))
        ));
    }

    let programs = usage.by_program();
    if !programs.is_empty() {
        out.push_str(&format!(
            "{}\n",
            ui::heading("by command, across every project")
        ));
        let top = programs.first().map(|(_, t)| t.saved()).unwrap_or(1).max(1);
        let rows: Vec<Vec<String>> = programs
            .iter()
            .take(10)
            .map(|(name, t)| {
                vec![
                    name.clone(),
                    ui::paint(ui::OK, m(t.saved())),
                    ui::bar(t.saved() as f64 / top as f64, 14),
                    format!("{} run{}", t.runs, if t.runs == 1 { "" } else { "s" }),
                ]
            })
            .collect();
        out.push_str(&ui::table(
            &["command", "saved", "", "runs"],
            &[Align::Left, Align::Right, Align::Left, Align::Right],
            &rows,
        ));
    }

    if usage.skipped_lines > 0 {
        out.push_str(&format!(
            "\n{}\n",
            ui::warn(format!(
                "{} unreadable record(s) skipped — the totals are that much low",
                usage.skipped_lines
            ))
        ));
    }
    if let Some(p) = &usage.path {
        out.push_str(&format!(
            "\n{}\n",
            ui::paint(
                ui::DIM,
                format!("ledger: {}  (local only, never sent anywhere)", p.display())
            )
        ));
    }
    if est {
        out.push_str(&format!(
            "{} {}\n",
            ui::paint(ui::DIM, ui::glyphs().bullet),
            ui::paint(
                ui::DIM,
                "`~` marks heuristic estimates: exact counts need a provider usage field."
            )
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// `ttk gain`: the running total
// ---------------------------------------------------------------------------

/// Everything `ttk gain` shows, gathered by the command so that rendering stays
/// a pure function of it.
pub struct GainView<'a> {
    /// Totals, already narrowed to one project when `--project` was given.
    pub usage: &'a ttk_store::GlobalUsage,
    /// Per-day totals, keyed by local day number.
    pub daily: &'a BTreeMap<i64, ttk_store::usage::ProgramTotals>,
    /// Today's local day number.
    pub today: i64,
    /// How many days the chart covers.
    pub days: usize,
    /// `all projects`, or the project's path.
    pub scope: String,
    /// Whether the view is the whole ledger (and so worth a project table).
    pub all_projects: bool,
    /// The global folder, and what is in it.
    pub home: Option<std::path::PathBuf>,
    pub filter_files: usize,
    pub filter_rules: usize,
}

impl GainView<'_> {
    /// Saved tokens and runs over the last `n` days, today included.
    fn window(&self, n: i64) -> ttk_store::usage::ProgramTotals {
        let mut t = ttk_store::usage::ProgramTotals::default();
        for (_, d) in self.daily.range(self.today - n + 1..=self.today) {
            t.runs += d.runs;
            t.tokens_before += d.tokens_before;
            t.tokens_after += d.tokens_after;
        }
        t
    }
}

/// Render `ttk gain`: one big number, then where it came from.
pub fn render_gain(v: &GainView) -> String {
    let usage = v.usage;
    let est = usage.method != "provider" && usage.method != "tokenizer";
    let m = |n: u64| ui::tokens(n, est);
    let rate = |t: &ttk_store::usage::ProgramTotals| {
        if t.tokens_before == 0 {
            "–".to_string()
        } else {
            format!("{:.0}%", t.saved() as f64 * 100.0 / t.tokens_before as f64)
        }
    };
    let mut out = String::new();

    out.push_str(&ui::banner(&format!(
        "token savings {} {}",
        ui::glyphs().bullet,
        v.scope
    )));
    out.push_str("\n\n");

    if usage.is_empty() {
        out.push_str(&format!(
            "  {}\n  {}\n\n",
            ui::paint(ui::HEAD, "Nothing saved yet."),
            ui::paint(
                ui::DIM,
                "Every `ttk run`, in any project, is counted here automatically."
            )
        ));
        out.push_str(&format!(
            "{}\n",
            ui::hint("try it ", "ttk run -- git status")
        ));
        out.push_str(&format!("{}\n", ui::hint("then   ", "ttk gain")));
        out.push_str(&render_gain_folder(v));
        return out;
    }

    // -- the headline ------------------------------------------------------
    out.push_str(&format!("  {}\n", ui::paint(ui::DIM, "TOKENS SAVED")));
    out.push_str(&format!("  {}\n", ui::paint(ui::HERO, m(usage.saved()))));
    out.push_str(&format!(
        "  {}  {}  {}\n",
        ui::bar(usage.saved_percent() / 100.0, 36),
        ui::paint(ui::HEAD, format!("{:.1}%", usage.saved_percent())),
        ui::paint(
            ui::DIM,
            format!("of {} tokens never reached a model", m(usage.tokens_before))
        )
    ));

    // -- by period ---------------------------------------------------------
    let all_time = ttk_store::usage::ProgramTotals {
        runs: usage.events,
        tokens_before: usage.tokens_before,
        tokens_after: usage.tokens_after,
    };
    let mut rows = Vec::new();
    for (label, t) in [
        ("today", v.window(1)),
        ("last 7 days", v.window(7)),
        ("last 30 days", v.window(30)),
        ("all time", all_time),
    ] {
        rows.push(vec![
            label.to_string(),
            if t.saved() == 0 {
                ui::paint(ui::DIM, m(0))
            } else {
                ui::paint(ui::SAVED, m(t.saved()))
            },
            ui::thousands(t.runs),
            rate(&t),
        ]);
    }
    out.push('\n');
    out.push_str(&ui::table(
        &["period", "saved", "runs", "rate"],
        &[Align::Left, Align::Right, Align::Right, Align::Right],
        &rows,
    ));

    let mut facts = vec![format!(
        "{} project{}",
        usage.projects.len(),
        if usage.projects.len() == 1 { "" } else { "s" }
    )];
    if let Some(first) = usage.first_millis {
        facts.push(format!(
            "since {}",
            ui::iso_date(ttk_store::usage::day_number(first, ui::local_offset_secs()))
        ));
    }
    if usage.filtered_lines > 0 {
        facts.push(format!(
            "{} lines removed by learned filters",
            ui::thousands(usage.filtered_lines)
        ));
    }
    out.push_str(&format!(
        "  {}\n",
        ui::paint(ui::DIM, facts.join(&format!(" {} ", ui::glyphs().bullet)))
    ));

    // -- the chart ---------------------------------------------------------
    let days = v.days.max(1) as i64;
    let series: Vec<(i64, ttk_store::usage::ProgramTotals)> = (v.today - days + 1..=v.today)
        .map(|d| (d, v.daily.get(&d).copied().unwrap_or_default()))
        .collect();
    let values: Vec<u64> = series.iter().map(|(_, t)| t.saved()).collect();
    let top = values.iter().copied().max().unwrap_or(0).max(1);
    let active = series.iter().any(|(_, t)| t.runs > 0);
    out.push_str(&ui::heading(&format!("last {days} days")));
    if active {
        out.push_str(&format!("  {}", ui::sparkline(&values)));
    }
    out.push('\n');
    if !active {
        // Fourteen empty rows say less than one line does.
        out.push_str(&format!(
            "  {}\n",
            ui::paint(ui::DIM, format!("no runs in the last {days} days"))
        ));
    }
    for (day, t) in series.iter().rev().filter(|_| active) {
        let label = if *day == v.today {
            ui::paint(ui::HEAD, format!("{:<9}", "today"))
        } else {
            ui::paint(ui::DIM, ui::short_day(*day))
        };
        if t.runs == 0 {
            out.push_str(&format!(
                "  {label}  {}\n",
                ui::paint(ui::DIM, ui::glyphs().bar_empty.repeat(28))
            ));
            continue;
        }
        out.push_str(&format!(
            "  {label}  {}  {:>12}  {}\n",
            ui::bar(t.saved() as f64 / top as f64, 28),
            ui::paint(ui::SAVED, m(t.saved())),
            ui::paint(
                ui::DIM,
                format!("{} run{}", t.runs, if t.runs == 1 { "" } else { "s" })
            )
        ));
    }

    // -- where it came from ------------------------------------------------
    let programs = usage.by_program();
    if !programs.is_empty() {
        out.push_str(&format!("{}\n", ui::heading("top commands")));
        let top = programs.first().map(|(_, t)| t.saved()).unwrap_or(1).max(1);
        let rows: Vec<Vec<String>> = programs
            .iter()
            .take(8)
            .map(|(name, t)| {
                vec![
                    ui::paint(ui::CODE, name),
                    ui::paint(ui::SAVED, m(t.saved())),
                    ui::bar(t.saved() as f64 / top as f64, 16),
                    rate(t),
                    ui::thousands(t.runs),
                ]
            })
            .collect();
        out.push_str(&ui::table(
            &["command", "saved", "", "rate", "runs"],
            &[
                Align::Left,
                Align::Right,
                Align::Left,
                Align::Right,
                Align::Right,
            ],
            &rows,
        ));
    }

    if v.all_projects && usage.projects.len() > 1 {
        out.push_str(&format!("{}\n", ui::heading("top projects")));
        let top = usage
            .projects
            .first()
            .map(|p| p.saved())
            .unwrap_or(1)
            .max(1);
        let rows: Vec<Vec<String>> = usage
            .projects
            .iter()
            .take(8)
            .map(|p| {
                vec![
                    ui::ellipsize(&short_path(&p.project), 40),
                    ui::paint(ui::SAVED, m(p.saved())),
                    ui::bar(p.saved() as f64 / top as f64, 16),
                    format!("{:.0}%", p.saved_percent()),
                    ui::thousands(p.events),
                ]
            })
            .collect();
        out.push_str(&ui::table(
            &["project", "saved", "", "rate", "runs"],
            &[
                Align::Left,
                Align::Right,
                Align::Left,
                Align::Right,
                Align::Right,
            ],
            &rows,
        ));
        if usage.projects.len() > 8 {
            out.push_str(&format!(
                "{}\n",
                ui::detail(format!("{} more project(s)", usage.projects.len() - 8))
            ));
        }
    }

    out.push_str(&render_gain_folder(v));

    if usage.skipped_lines > 0 {
        out.push_str(&format!(
            "\n{}\n",
            ui::warn(format!(
                "{} unreadable record(s) skipped — the totals are that much low",
                usage.skipped_lines
            ))
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "  {}  {}  {}\n",
        ui::paint(ui::CODE, "ttk gain --project"),
        ui::paint(ui::CODE, "ttk gain --days 30"),
        ui::paint(ui::CODE, "ttk global --open"),
    ));
    if est {
        out.push_str(&format!(
            "  {}\n",
            ui::paint(
                ui::DIM,
                "`~` marks heuristic estimates; exact counts need a provider usage field."
            )
        ));
    }
    out
}

fn render_gain_folder(v: &GainView) -> String {
    let Some(home) = &v.home else {
        return String::new();
    };
    let mut out = format!("{}\n", ui::heading("global folder"));
    out.push_str(&format!(
        "{}\n",
        ui::kv("folder", ui::paint(ui::CODE, home.display()))
    ));
    out.push_str(&format!(
        "{}\n",
        ui::kv_note(
            "global filters",
            format!(
                "{} rule{} in {} file{}",
                v.filter_rules,
                if v.filter_rules == 1 { "" } else { "s" },
                v.filter_files,
                if v.filter_files == 1 { "" } else { "s" }
            ),
            format!(
                "{}{}",
                ttk_core::config::GLOBAL_FILTERS_DIR,
                std::path::MAIN_SEPARATOR
            )
        )
    ));
    out.push_str(&format!(
        "{}\n",
        ui::kv_note(
            "savings ledger",
            ttk_store::usage::USAGE_FILE,
            "every run in every project, local only"
        )
    ));
    out
}

/// `/home/me/work/api` → `~/work/api`, and a Windows path likewise.
///
/// Shortening is cosmetic, so it gives up rather than guessing whenever the
/// home directory is not a prefix.
pub(crate) fn short_path(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home = home.display().to_string();
    match path.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
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

/// The one line summary printed on stderr after `ttk run` / `ttk compile`.
///
/// It goes to stderr on purpose: stdout carries the compiled output, and a
/// summary mixed into it would end up inside whatever the caller pipes it to.
pub fn one_line(before: TokenCount, after: TokenCount, capsule: Option<&str>) -> String {
    let saved = before.saved_against(after);
    let pct = if before.value == 0 {
        0.0
    } else {
        saved.value as f64 * 100.0 / before.value as f64
    };
    let arrow = ui::glyphs().arrow;
    let head = format!(
        "{}  {} {arrow} {} tokens  {}  {}",
        ui::paint(ui::ACCENT, "ttk"),
        before,
        ui::paint(ui::NUM, after),
        ui::bar(pct / 100.0, 16),
        ui::paint(
            if pct >= 25.0 { ui::OK } else { ui::DIM },
            format!("-{pct:.0}%")
        ),
    );
    match capsule {
        Some(c) => format!("{head}  {}", ui::paint(ui::DIM, c)),
        None => head,
    }
}

/// Why the output looks the way it does, when it is not just "a compiler ran".
///
/// A pointer to an earlier run is surprising enough that it has to say so
/// unprompted: an agent seeing `[repeat]` for the first time should not have to
/// guess whether its command actually ran.
pub fn winner_line(winner: &str) -> Option<String> {
    let note = match winner {
        "repeat.suppress" => "the command ran and produced byte-identical output to an earlier run",
        "delta.lines" => "only what changed since the earlier run is shown",
        _ => return None,
    };
    Some(format!("     {}", ui::paint(ui::DIM, note)))
}

/// The second stderr line, present only when learned rules actually fired.
pub fn filter_line(filtered: &ttk_learn::Filtered) -> String {
    format!(
        "     {}",
        ui::paint(
            ui::DIM,
            format!(
                "learned filter: -{} line(s) via {} rule(s) — ttk rules",
                filtered.removed_lines,
                filtered.by_rule.len()
            )
        )
    )
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
        assert!(r.render().contains("fell back x1"), "{}", r.render());
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
        assert!(r.render().contains("tokens"), "{}", r.render());
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
        assert!(text.contains("nothing recorded"), "{text}");
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
    fn the_global_report_leads_with_one_number() {
        let mut usage = ttk_store::GlobalUsage {
            events: 3,
            tokens_before: 1000,
            tokens_after: 100,
            method: "estimated".into(),
            ..Default::default()
        };
        usage.projects.push(ttk_store::ProjectUsage {
            project: "/work/api".into(),
            project_id: "abc".into(),
            events: 3,
            tokens_before: 1000,
            tokens_after: 100,
            ..Default::default()
        });
        let text = render_global(&usage);
        assert!(text.contains("everywhere"), "{text}");
        assert!(text.contains("by project"), "{text}");
        assert!(text.contains("/work/api"), "{text}");
        assert!(text.contains('~'), "estimates stay marked: {text}");
    }

    #[test]
    fn an_empty_global_report_says_how_to_fill_it() {
        let text = render_global(&ttk_store::GlobalUsage::default());
        assert!(text.contains("ttk run"), "{text}");
        assert!(!text.contains("by project"), "no empty table: {text}");
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
        assert!(s.contains('~'), "an estimate stays marked as one");
    }

    #[test]
    fn a_zero_token_input_does_not_divide_by_zero() {
        let s = one_line(TokenCount::estimated(0), TokenCount::estimated(0), None);
        assert!(s.contains("-0%"), "{s}");
    }
}
