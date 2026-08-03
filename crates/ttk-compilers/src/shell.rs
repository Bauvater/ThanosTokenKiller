//! Generic shell output compilers.
//!
//! These run last, after every specialized compiler declined. They only apply
//! transformations that are safe for *unknown* content:
//!
//! * collapsing consecutive identical lines into `… (xN)`,
//! * grouping `path:line:match` output (grep/rg) by file,
//! * head/tail truncation for very long output — flagged lossy, capsule kept.

use std::sync::LazyLock;

use regex::Regex;
use ttk_core::firewall::{Candidate, Reparse};
use ttk_core::invariants::{Invariant, InvariantKind};
use ttk_core::ir::{IrDoc, IrSection};

use crate::{CompileInput, Compiler, clip, fmt_duration_ms, lines_capped};

const MAX_LINES: usize = 500_000;
/// Output longer than this gets head/tail treatment.
const TRUNCATE_ABOVE: usize = 200;
const HEAD_LINES: usize = 60;
const TAIL_LINES: usize = 40;
const MAX_FILES: usize = 200;
const MAX_MATCHES_PER_FILE: usize = 20;

// ---------------------------------------------------------------------------
// grep / ripgrep
// ---------------------------------------------------------------------------

static GREP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^((?:[A-Za-z]:)?[^:\n]+):(\d+):(?:(\d+):)?(.*)$").expect("static regex")
});

/// Matches of one file, plus how many were dropped by the per-file cap.
struct FileMatches {
    path: String,
    matches: Vec<(String, String)>,
    dropped: usize,
}

pub struct GrepCompiler;

impl Compiler for GrepCompiler {
    fn id(&self) -> &'static str {
        "shell.grep"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        if matches!(
            input.program().as_deref(),
            Some("grep") | Some("rg") | Some("ripgrep")
        ) {
            return true;
        }
        // Otherwise require that most lines look like `path:line:content`.
        let mut hits = 0;
        let mut total = 0;
        for line in lines_capped(input.content, 40) {
            if line.trim().is_empty() {
                continue;
            }
            total += 1;
            if GREP_RE.is_match(line) {
                hits += 1;
            }
        }
        total >= 5 && hits * 4 >= total * 3
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut files: Vec<FileMatches> = Vec::new();
        let mut total = 0usize;

        for line in lines_capped(input.content, MAX_LINES) {
            let Some(caps) = GREP_RE.captures(line) else {
                continue;
            };
            total += 1;
            let path = caps[1].to_string();
            let lineno = caps[2].to_string();
            let content = caps[4].to_string();
            match files.iter_mut().find(|f| f.path == path) {
                Some(f) => {
                    if f.matches.len() < MAX_MATCHES_PER_FILE {
                        f.matches.push((lineno, content));
                    } else {
                        f.dropped += 1;
                    }
                }
                None => {
                    if files.len() < MAX_FILES {
                        files.push(FileMatches {
                            path,
                            matches: vec![(lineno, content)],
                            dropped: 0,
                        });
                    }
                }
            }
        }
        // Grouping only pays off when paths actually repeat. With roughly one
        // match per file the rendering would be as long as the input, and the
        // firewall would reject it for lack of gain anyway.
        if files.is_empty() || total < 3 || total < files.len() * 2 {
            return None;
        }

        let mut doc = IrDoc::new("shell")
            .variant("grep")
            .head("matches", total.to_string())
            .field_num("files", files.len());
        let mut invariants = Vec::new();
        let mut dropped_any = false;
        for f in &files {
            let mut sec = IrSection::new(f.path.clone()).attr("n", f.matches.len().to_string());
            if f.dropped > 0 {
                dropped_any = true;
                sec = sec.attr("not_shown", f.dropped.to_string());
            }
            for (lineno, content) in &f.matches {
                sec = sec.line(format!("{lineno}: {}", clip(content.trim_end(), 300)));
            }
            doc = doc.section(sec);
        }
        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }

        // The rendering splits `path:line` into a section label plus a
        // `line:` prefix, so only the path itself is literally present.
        for f in &files {
            invariants.push(Invariant::new(InvariantKind::Path, f.path.clone()));
        }

        Some(
            Candidate::new(self.id(), self.version(), doc.render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr)
                .lossy(dropped_any)
                .note("matches grouped by file"),
        )
    }
}

// ---------------------------------------------------------------------------
// generic fallback
// ---------------------------------------------------------------------------

pub struct GenericShellCompiler;

impl Compiler for GenericShellCompiler {
    fn id(&self) -> &'static str {
        "shell.generic"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        input.command.is_some() && !input.content.trim().is_empty()
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let cmd = input.command?;
        let lines: Vec<&str> = lines_capped(input.content, MAX_LINES).collect();

        // Collapse consecutive duplicates. Fully reversible from the counts.
        let mut collapsed: Vec<String> = Vec::with_capacity(lines.len());
        let mut i = 0usize;
        let mut collapsed_lines = 0usize;
        while i < lines.len() {
            let mut run = 1usize;
            while i + run < lines.len() && lines[i + run] == lines[i] {
                run += 1;
            }
            if run > 1 {
                collapsed.push(format!("{} (x{run})", lines[i].trim_end()));
                collapsed_lines += run - 1;
            } else {
                collapsed.push(lines[i].trim_end().to_string());
            }
            i += run;
        }

        let truncated = collapsed.len() > TRUNCATE_ABOVE;
        let kept: Vec<String> = if truncated {
            let mut v: Vec<String> = collapsed[..HEAD_LINES].to_vec();
            v.push(format!(
                "… {} lines omitted, retrieve with `ttk retrieve <capsule> --level 4` …",
                collapsed.len() - HEAD_LINES - TAIL_LINES
            ));
            v.extend_from_slice(&collapsed[collapsed.len() - TAIL_LINES..]);
            v
        } else {
            collapsed.clone()
        };

        let mut doc = IrDoc::new("shell")
            .variant("out")
            .head("cmd", cmd.command_line())
            .field_num("exit", cmd.exit_code.unwrap_or(-1))
            .field("duration", fmt_duration_ms(cmd.duration_ms))
            .field_num("lines", lines.len());
        if let Some(sig) = cmd.signal {
            doc = doc.field_num("signal", sig);
        }
        if collapsed_lines > 0 {
            doc = doc.field_num("duplicate_lines_collapsed", collapsed_lines);
        }
        if truncated {
            doc = doc.field_num("lines_not_shown", collapsed.len() - HEAD_LINES - TAIL_LINES);
        }
        doc = doc.section(IrSection::new("out").lines(kept.iter().map(|l| clip(l, 500))));
        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }

        let mut invariants = vec![Invariant::new(InvariantKind::Command, cmd.command_line())];
        // Error-shaped lines must survive truncation.
        let critical = ttk_core::invariants::critical_regions(input.content);
        invariants.extend(ttk_core::invariants::extract(
            &critical,
            &ttk_core::invariants::ExtractPolicy::critical(),
        ));
        invariants.sort();
        invariants.dedup();

        Some(
            Candidate::new(self.id(), self.version(), doc.render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr)
                .lossy(truncated)
                .note(if truncated {
                    "output truncated to head+tail; full output in the capsule"
                } else {
                    "consecutive duplicate lines collapsed"
                }),
        )
    }
}
