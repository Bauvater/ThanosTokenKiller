//! Log compiler.
//!
//! Application logs are dominated by repetition. This compiler normalises each
//! line into a *template* (numbers, hex ids, uuids, quoted strings and
//! timestamps become placeholders), groups identical templates and reports
//! count, first and last occurrence plus one verbatim example.
//!
//! The algorithm is deliberately deterministic and dependency free — a
//! Drain-style tokeniser without the tree, which is enough for the
//! "same message, different id" case that dominates real logs. No machine
//! learning, no similarity thresholds, no surprises.
//!
//! Clustering cannot be reversed from the rendering alone, so the candidate is
//! flagged lossy and only accepted in `balanced` and `maximum` mode. The full
//! log always stays in the capsule.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use ttk_core::firewall::{Candidate, Reparse};
use ttk_core::invariants::ExtractPolicy;
use ttk_core::ir::{IrDoc, IrSection};

use crate::{CompileInput, Compiler, clip, lines_capped};

const MAX_LINES: usize = 500_000;
const MAX_CLUSTERS: usize = 40;
/// Below this many lines clustering is not worth it.
const MIN_LINES: usize = 12;

static NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("static regex"));
static HEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:0x)?[0-9a-fA-F]{8,}\b").expect("static regex"));
static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
        .expect("static regex")
});
static QUOTED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""[^"]*"|'[^']*'"#).expect("static regex"));
static TS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
        .expect("static regex")
});
static LEVEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|FATAL|CRITICAL|PANIC)\b")
        .expect("static regex")
});

/// Replace volatile parts of a log line to obtain its template.
pub fn template_of(line: &str) -> String {
    let s = TS_RE.replace_all(line, "<ts>");
    let s = UUID_RE.replace_all(&s, "<uuid>");
    let s = QUOTED_RE.replace_all(&s, "<str>");
    let s = HEX_RE.replace_all(&s, "<hex>");
    let s = NUM_RE.replace_all(&s, "<n>");
    s.trim().to_string()
}

fn level_of(line: &str) -> Option<String> {
    LEVEL_RE.captures(line).map(|c| c[1].to_ascii_uppercase())
}

#[derive(Debug)]
struct Cluster {
    template: String,
    count: u64,
    first_line: usize,
    last_line: usize,
    example: String,
    level: Option<String>,
}

pub struct LogCompiler;

impl Compiler for LogCompiler {
    fn id(&self) -> &'static str {
        "log.cluster"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        use ttk_core::content::ContentType;
        let log_like = input.content_type == ContentType::ApplicationLog;
        // Output of a command we ran keeps its exit code and command line via
        // the shell compiler unless it really is a log stream (`docker logs`,
        // `kubectl logs`, a tailed file).
        if input.command.is_some() && !log_like {
            return false;
        }
        (log_like
            || matches!(
                input.content_type,
                ContentType::ShellOutput | ContentType::Unknown | ContentType::PlainText
            ))
            && input.content.lines().take(MIN_LINES + 1).count() > MIN_LINES
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut clusters: HashMap<String, Cluster> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut total = 0usize;

        for (i, line) in lines_capped(input.content, MAX_LINES).enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            total += 1;
            let template = template_of(line);
            match clusters.get_mut(&template) {
                Some(c) => {
                    c.count += 1;
                    c.last_line = i + 1;
                }
                None => {
                    order.push(template.clone());
                    clusters.insert(
                        template.clone(),
                        Cluster {
                            template,
                            count: 1,
                            first_line: i + 1,
                            last_line: i + 1,
                            example: clip(line.trim(), 400),
                            level: level_of(line),
                        },
                    );
                }
            }
        }

        if total < MIN_LINES || clusters.is_empty() {
            return None;
        }
        // Nothing repeats: clustering would only add noise.
        if clusters.len() * 2 > total {
            return None;
        }

        let mut list: Vec<&Cluster> = order.iter().filter_map(|t| clusters.get(t)).collect();
        // Errors first, then by frequency, then by first appearance: stable and
        // explainable.
        list.sort_by(|a, b| {
            severity(a)
                .cmp(&severity(b))
                .reverse()
                .then(b.count.cmp(&a.count))
                .then(a.first_line.cmp(&b.first_line))
        });

        let shown = list.len().min(MAX_CLUSTERS);
        let mut doc = IrDoc::new("log")
            .variant("cluster")
            .head("lines", total.to_string())
            .field_num("templates", list.len());
        if list.len() > shown {
            let hidden: u64 = list[shown..].iter().map(|c| c.count).sum();
            doc = doc
                .field_num("templates_not_shown", list.len() - shown)
                .field_num("lines_not_shown", hidden);
        }

        let mut invariants = Vec::new();
        for c in list.iter().take(shown) {
            let mut sec = IrSection::new(format!("t{}", c.first_line))
                .attr("count", c.count.to_string())
                .attr("lines", format!("{}-{}", c.first_line, c.last_line));
            if let Some(l) = &c.level {
                sec = sec.attr("level", l.clone());
            }
            sec = sec.line(c.example.clone());
            if c.count > 1 && c.template != c.example {
                sec = sec.line(format!("template: {}", clip(&c.template, 300)));
            }
            // Error-shaped examples carry invariants we must not lose.
            if severity(c) > 0 {
                invariants.extend(ttk_core::invariants::extract(
                    &c.example,
                    &ExtractPolicy::critical(),
                ));
            }
            doc = doc.section(sec);
        }
        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }

        invariants.sort();
        invariants.dedup();
        Some(
            Candidate::new(self.id(), self.version(), doc.render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr)
                .lossy(true)
                .note(format!(
                    "{total} log lines grouped into {} templates",
                    list.len()
                )),
        )
    }
}

fn severity(c: &Cluster) -> u8 {
    match c.level.as_deref() {
        Some("FATAL") | Some("CRITICAL") | Some("PANIC") => 3,
        Some("ERROR") => 2,
        Some("WARN") | Some("WARNING") => 1,
        _ => 0,
    }
}
