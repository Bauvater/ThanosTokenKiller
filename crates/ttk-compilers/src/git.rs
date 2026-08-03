//! Git output compilers.
//!
//! `git status`, `git diff` and `git log` are the three commands an agent runs
//! most often, and all three are extremely verbose relative to their
//! information content.
//!
//! The diff compiler is the interesting one: it keeps **every changed line and
//! every hunk header verbatim** and only drops unchanged context, which the
//! capsule still holds. Nothing about the patch semantics is guessed.

use std::sync::LazyLock;

use regex::Regex;
use ttk_core::firewall::{Candidate, Reparse};
use ttk_core::invariants::{Invariant, InvariantKind};
use ttk_core::ir::{IrDoc, IrSection};

use crate::{CompileInput, Compiler, clip, lines_capped};

const MAX_LINES: usize = 500_000;
const MAX_FILES: usize = 200;
const MAX_CHANGED_LINES_PER_FILE: usize = 400;

fn is_git(input: &CompileInput<'_>, sub: &str) -> bool {
    input.program().as_deref() == Some("git") && input.argv().iter().skip(1).any(|a| a == sub)
}

fn finish(doc: IrDoc, input: &CompileInput<'_>) -> IrDoc {
    match input.capsule_ref {
        Some(r) => doc.raw_capsule(r.trim_start_matches("cap://")),
        None => doc,
    }
}

// ---------------------------------------------------------------------------
// git status
// ---------------------------------------------------------------------------

static BRANCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^On branch (\S+)").expect("static regex"));
static AHEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ahead of '([^']+)' by (\d+) commit").expect("static regex"));
static BEHIND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"behind '([^']+)' by (\d+) commit").expect("static regex"));

pub struct GitStatusCompiler;

impl Compiler for GitStatusCompiler {
    fn id(&self) -> &'static str {
        "git.status"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        is_git(input, "status")
            || input.content.starts_with("On branch ")
            || input
                .content
                .contains("nothing to commit, working tree clean")
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut doc = IrDoc::new("git").variant("status");
        let mut entries: Vec<(String, String)> = Vec::new();
        let mut section = "";
        let mut branch = None;
        let mut ahead = None;
        let mut behind = None;
        let mut clean = false;

        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(c) = BRANCH_RE.captures(line) {
                branch = Some(c[1].to_string());
                continue;
            }
            if let Some(c) = AHEAD_RE.captures(line) {
                ahead = Some(c[2].to_string());
            }
            if let Some(c) = BEHIND_RE.captures(line) {
                behind = Some(c[2].to_string());
            }
            if line.contains("nothing to commit") {
                clean = true;
            }
            match line.trim_end() {
                "Changes to be committed:" => section = "staged",
                "Changes not staged for commit:" => section = "unstaged",
                "Untracked files:" => section = "untracked",
                _ => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || line.starts_with("  (") || section.is_empty() {
                continue;
            }
            // Long format entries are indented by a tab or spaces.
            if !(line.starts_with('\t') || line.starts_with("        ")) {
                continue;
            }
            let (state, path) = match trimmed.split_once(':') {
                Some((s, p)) if !s.contains(' ') || s.len() <= 12 => {
                    (s.trim().to_string(), p.trim().to_string())
                }
                _ => ("untracked".to_string(), trimmed.to_string()),
            };
            if entries.len() < MAX_FILES {
                entries.push((format!("{section}/{state}"), path));
            }
        }

        // Porcelain format: `XY path`.
        if entries.is_empty() {
            for line in lines_capped(input.content, MAX_LINES) {
                if line.len() < 4 || !line.is_char_boundary(3) {
                    continue;
                }
                let (code, path) = line.split_at(3);
                let code = code.trim();
                if code.is_empty()
                    || code.len() > 2
                    || !code.chars().all(|c| "MADRCU?!".contains(c))
                {
                    continue;
                }
                if entries.len() < MAX_FILES {
                    entries.push((code.to_string(), path.trim().to_string()));
                }
            }
        }

        if entries.is_empty() && branch.is_none() && !clean {
            return None;
        }

        doc = doc.head(
            "status",
            if clean && entries.is_empty() {
                "clean"
            } else {
                "dirty"
            },
        );
        if let Some(b) = &branch {
            doc = doc.field("branch", b.clone());
        }
        if let Some(a) = ahead {
            doc = doc.field("ahead", a);
        }
        if let Some(b) = behind {
            doc = doc.field("behind", b);
        }
        doc = doc.field_num("files", entries.len());

        let mut invariants = Vec::new();
        if !entries.is_empty() {
            let mut sec = IrSection::new("files");
            for (state, path) in &entries {
                sec = sec.line(format!("{state} {path}"));
                invariants.push(Invariant::new(InvariantKind::Path, path.clone()));
            }
            doc = doc.section(sec);
        }

        Some(
            Candidate::new(self.id(), self.version(), finish(doc, input).render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr),
        )
    }
}

// ---------------------------------------------------------------------------
// git diff / git show
// ---------------------------------------------------------------------------

static DIFF_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^diff --git a/(.+) b/(.+)$").expect("static regex"));

#[derive(Default)]
struct FileDiff {
    path: String,
    added: usize,
    removed: usize,
    binary: bool,
    new_file: bool,
    deleted: bool,
    renamed_from: Option<String>,
    /// Hunk headers and changed lines, verbatim and in order.
    kept: Vec<String>,
    context_lines: usize,
    truncated: usize,
}

pub struct GitDiffCompiler;

impl Compiler for GitDiffCompiler {
    fn id(&self) -> &'static str {
        "git.diff"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        input.content.contains("diff --git ")
            || (input.content.starts_with("--- ") && input.content.contains("\n@@ "))
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut files: Vec<FileDiff> = Vec::new();
        let mut cur: Option<FileDiff> = None;

        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(caps) = DIFF_HEADER_RE.captures(line) {
                if let Some(f) = cur.take() {
                    files.push(f);
                }
                cur = Some(FileDiff {
                    path: caps[2].to_string(),
                    ..Default::default()
                });
                continue;
            }
            let Some(f) = cur.as_mut() else { continue };

            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                f.binary = true;
            } else if line.starts_with("new file mode") {
                f.new_file = true;
            } else if line.starts_with("deleted file mode") {
                f.deleted = true;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                f.renamed_from = Some(from.to_string());
            } else if line.starts_with("@@") {
                keep(f, line);
            } else if line.starts_with("+++")
                || line.starts_with("---")
                || line.starts_with("index ")
            {
                // Redundant with the `diff --git` header.
                continue;
            } else if let Some(rest) = line.strip_prefix('+') {
                f.added += 1;
                let _ = rest;
                keep(f, line);
            } else if let Some(rest) = line.strip_prefix('-') {
                f.removed += 1;
                let _ = rest;
                keep(f, line);
            } else {
                f.context_lines += 1;
            }
        }
        if let Some(f) = cur.take() {
            files.push(f);
        }
        if files.is_empty() {
            return None;
        }

        let total_added: usize = files.iter().map(|f| f.added).sum();
        let total_removed: usize = files.iter().map(|f| f.removed).sum();
        let shown = files.len().min(MAX_FILES);

        let mut doc = IrDoc::new("git")
            .variant("diff")
            .head("files", files.len().to_string())
            .field_num("added", total_added)
            .field_num("removed", total_removed);
        if files.len() > shown {
            doc = doc.field_num("files_not_shown", files.len() - shown);
        }

        let mut invariants = Vec::new();
        for f in files.iter().take(shown) {
            let mut sec = IrSection::new(f.path.clone())
                .attr("+", f.added.to_string())
                .attr("-", f.removed.to_string());
            if f.binary {
                sec = sec.attr("binary", "true");
            }
            if f.new_file {
                sec = sec.attr("new", "true");
            }
            if f.deleted {
                sec = sec.attr("deleted", "true");
            }
            if let Some(from) = &f.renamed_from {
                sec = sec.attr("renamed_from", from.clone());
            }
            if f.context_lines > 0 {
                sec = sec.attr("context_dropped", f.context_lines.to_string());
            }
            if f.truncated > 0 {
                sec = sec.attr("changed_lines_not_shown", f.truncated.to_string());
            }
            sec = sec.lines(f.kept.iter().map(|l| clip(l, 500)));
            invariants.push(Invariant::new(InvariantKind::Path, f.path.clone()));
            doc = doc.section(sec);
        }

        let truncated_any = files.iter().any(|f| f.truncated > 0) || files.len() > shown;
        Some(
            Candidate::new(self.id(), self.version(), finish(doc, input).render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr)
                .lossy(truncated_any)
                .note("unchanged context lines dropped; full diff in the capsule"),
        )
    }
}

fn keep(f: &mut FileDiff, line: &str) {
    if f.kept.len() < MAX_CHANGED_LINES_PER_FILE {
        f.kept.push(line.to_string());
    } else {
        f.truncated += 1;
    }
}

// ---------------------------------------------------------------------------
// git log
// ---------------------------------------------------------------------------

static COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^commit ([0-9a-f]{7,40})").expect("static regex"));
static AUTHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Author:\s+(.+?)\s+<").expect("static regex"));
static DATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Date:\s+(.+)$").expect("static regex"));

pub struct GitLogCompiler;

impl Compiler for GitLogCompiler {
    fn id(&self) -> &'static str {
        "git.log"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        is_git(input, "log") || COMMIT_RE.is_match(input.content)
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        struct Commit {
            hash: String,
            author: String,
            date: String,
            subject: String,
        }
        let mut commits: Vec<Commit> = Vec::new();

        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(c) = COMMIT_RE.captures(line) {
                commits.push(Commit {
                    hash: c[1].to_string(),
                    author: String::new(),
                    date: String::new(),
                    subject: String::new(),
                });
                continue;
            }
            let Some(last) = commits.last_mut() else {
                continue;
            };
            if let Some(c) = AUTHOR_RE.captures(line) {
                last.author = c[1].to_string();
            } else if let Some(c) = DATE_RE.captures(line) {
                last.date = c[1].trim().to_string();
            } else if last.subject.is_empty() && line.starts_with("    ") {
                last.subject = line.trim().to_string();
            }
        }
        if commits.is_empty() {
            return None;
        }

        let mut sec = IrSection::new("commits");
        let mut invariants = Vec::new();
        for c in commits.iter().take(MAX_FILES) {
            let short: String = c.hash.chars().take(8).collect();
            sec = sec.line(format!(
                "{short} {} | {} | {}",
                c.date,
                c.author,
                clip(&c.subject, 120)
            ));
            invariants.push(Invariant::new(InvariantKind::Hash, short));
        }
        let doc = IrDoc::new("git")
            .variant("log")
            .head("commits", commits.len().to_string())
            .section(sec);

        Some(
            Candidate::new(self.id(), self.version(), finish(doc, input).render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr)
                .lossy(commits.len() > MAX_FILES),
        )
    }
}
