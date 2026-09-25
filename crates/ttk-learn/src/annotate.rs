//! Reading an agent's annotation.
//!
//! The whole teaching interface is two tags an agent can type without any
//! escaping rules to remember:
//!
//! ```text
//! keep this line
//! <filter-trash>
//! throw this one away
//! and this one
//! </filter-trash>
//! keep this too
//! ```
//!
//! Everything inside a `<filter-trash>` block is a **positive** example: teach
//! a rule that removes it. Everything outside is a **negative** example: no
//! rule may ever be learned that would also match it. That second half is what
//! makes the interface safe — the agent hands over the counter-examples for
//! free, simply by pasting back the output it saw.
//!
//! Three more tags exist because "delete it" is not always the answer:
//!
//! * `<filter-fold as="npm: deprecation warnings">…</filter-fold>` replaces the
//!   run with that one line. Forty warnings become six tokens *and* the fact
//!   that there were forty.
//! * `<filter-only>…</filter-only>` inverts the whole thing: these lines are
//!   the interesting ones, everything else this command prints can go. For
//!   output that is 99% noise, teaching the exception is far cheaper than
//!   teaching the rule.
//! * `<filter-keep>` is the undo: mark something a rule wrongly removed and
//!   that rule retires (see [`crate::RuleSet::retire_matching`]).

/// What a tag says about the lines inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Remove these.
    Trash,
    /// Replace the run with one summary line.
    Fold,
    /// These are the only lines worth keeping for this command.
    Only,
    /// This was removed and should not have been: retire whatever did it.
    Undo,
    /// Not marked at all. Evidence about what a rule may not touch.
    Plain,
}

/// A tag pair recognised by the parser.
struct Tag {
    /// Matched as a prefix, so `<filter-fold as="…">` is the same tag as
    /// `<filter-fold>`; anything up to the closing `>` is the attribute.
    open: &'static str,
    close: &'static str,
    kind: Kind,
}

const TAGS: &[Tag] = &[
    Tag {
        open: "<filter-trash",
        close: "</filter-trash>",
        kind: Kind::Trash,
    },
    // Short aliases, because an agent typing these by hand should not have to.
    Tag {
        open: "<trash",
        close: "</trash>",
        kind: Kind::Trash,
    },
    Tag {
        open: "<filter-fold",
        close: "</filter-fold>",
        kind: Kind::Fold,
    },
    Tag {
        open: "<fold",
        close: "</fold>",
        kind: Kind::Fold,
    },
    Tag {
        open: "<filter-only",
        close: "</filter-only>",
        kind: Kind::Only,
    },
    Tag {
        open: "<only",
        close: "</only>",
        kind: Kind::Only,
    },
    Tag {
        open: "<filter-keep",
        close: "</filter-keep>",
        kind: Kind::Undo,
    },
    Tag {
        open: "<keep",
        close: "</keep>",
        kind: Kind::Undo,
    },
];

/// A run marked with `<filter-fold as="…">`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldGroup {
    /// What the agent said this run amounts to. Empty when it said nothing,
    /// which the rule builder treats as a plain trash run.
    pub summary: String,
    pub lines: Vec<String>,
}

/// The result of reading an annotated document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Annotated {
    /// Lines marked for removal, in order, duplicates kept: repetition is
    /// evidence and the rule builder counts it.
    pub trash: Vec<String>,
    /// The same lines, grouped into *runs*: maximal stretches of marked text
    /// with no unmarked line between them.
    ///
    /// Contiguity is the whole input to block rules. A banner is a run; three
    /// deprecation warnings scattered through the output are three runs of one
    /// line each, and nothing may treat them as a block.
    pub runs: Vec<Vec<String>>,
    /// Lines that must never be removed: everything outside a trash block,
    /// plus everything inside a `<filter-keep>` block.
    pub keep: Vec<String>,
    /// Only the lines a `<filter-keep>` block marked explicitly. Unlike
    /// `keep`, these are an instruction as well as a counter-example: whatever
    /// rule removes one of them is wrong and gets retired.
    pub explicit_keep: Vec<String>,
    /// Runs marked with `<filter-fold as="…">`, in order.
    pub folds: Vec<FoldGroup>,
    /// Lines marked with `<filter-only>`: the ones worth keeping, when the
    /// lesson chose to teach the exception rather than the rule.
    pub only: Vec<String>,
    /// How many trash blocks were opened.
    pub blocks: usize,
    /// An opening tag without its closing partner. The block still counts —
    /// silently dropping the agent's work would be worse — but the caller is
    /// expected to say so.
    pub unclosed: bool,
}

impl Annotated {
    /// Does this annotation say anything at all?
    pub fn is_empty(&self) -> bool {
        self.trash.is_empty()
            && self.only.is_empty()
            && self.explicit_keep.is_empty()
            && self.folds.iter().all(|f| f.lines.is_empty())
    }
}

/// Split `text` into marked and unmarked lines.
///
/// Tags are recognised on their own line and inline (`<trash>noise</trash>`).
/// A line that carries only a tag contributes nothing itself. Nesting is not
/// supported and not needed: an inner tag is treated as plain text.
pub fn parse(text: &str) -> Annotated {
    let mut out = Annotated::default();
    // `None` = outside any block; `Some(i)` = inside TAGS[i].
    let mut open: Option<usize> = None;
    // The `as="…"` of the fold block currently open.
    let mut summary = String::new();
    // Whether the previous line that carried content was marked as trash, so a
    // run can be continued rather than restarted. Blank lines are invisible to
    // this: they neither start nor end a run.
    let mut in_run = false;

    for raw in text.lines() {
        let line = raw;
        let trimmed = line.trim();

        if let Some(i) = open {
            let tag = &TAGS[i];
            if let Some(idx) = trimmed.find(tag.close) {
                push(&mut out, &mut in_run, tag.kind, &summary, &trimmed[..idx]);
                open = None;
                summary.clear();
                // Text after a closing tag on the same line is unmarked.
                push(
                    &mut out,
                    &mut in_run,
                    Kind::Plain,
                    "",
                    &trimmed[idx + tag.close.len()..],
                );
            } else {
                push(&mut out, &mut in_run, tag.kind, &summary, line);
            }
            continue;
        }

        match find_open(trimmed) {
            Some(Open {
                index,
                at,
                body_at,
                attribute,
            }) => {
                let tag = &TAGS[index];
                if tag.kind == Kind::Trash {
                    out.blocks += 1;
                }
                if tag.kind == Kind::Fold {
                    // A new fold group starts even if the previous one ended on
                    // the line before: two folds are two summaries.
                    out.folds.push(FoldGroup {
                        summary: attribute.clone(),
                        lines: Vec::new(),
                    });
                    summary = attribute;
                }
                // Anything before the opening tag is unmarked.
                push(&mut out, &mut in_run, Kind::Plain, "", &trimmed[..at]);
                let rest = &trimmed[body_at..];
                match rest.find(tag.close) {
                    // Inline: `<trash>noise</trash>` on one line.
                    Some(end) => {
                        push(&mut out, &mut in_run, tag.kind, &summary, &rest[..end]);
                        summary.clear();
                        push(
                            &mut out,
                            &mut in_run,
                            Kind::Plain,
                            "",
                            &rest[end + tag.close.len()..],
                        );
                    }
                    None => {
                        push(&mut out, &mut in_run, tag.kind, &summary, rest);
                        open = Some(index);
                    }
                }
            }
            None => push(&mut out, &mut in_run, Kind::Plain, "", line),
        }
    }

    out.unclosed = open.is_some();
    out
}

/// An opening tag found on a line.
struct Open {
    index: usize,
    /// Byte offset of the `<`.
    at: usize,
    /// Byte offset just after the closing `>`.
    body_at: usize,
    /// The `as="…"` value, empty when there is none.
    attribute: String,
}

/// Find the first opening tag on `line`, with its attribute.
///
/// Tags are matched as a prefix and the parser then scans to the closing `>`,
/// which is what lets `<filter-fold as="npm noise">` and `<filter-fold>` be the
/// same tag without a second entry in the table.
fn find_open(line: &str) -> Option<Open> {
    let mut best: Option<Open> = None;
    for (index, tag) in TAGS.iter().enumerate() {
        let Some(at) = line.find(tag.open) else {
            continue;
        };
        let after = at + tag.open.len();
        // `<trash` must not match `<trashcan>`: the next character has to end
        // the tag name.
        let rest = &line[after..];
        let Some(close) = rest.find('>') else {
            continue;
        };
        let inside = &rest[..close];
        if !inside.is_empty() && !inside.starts_with(char::is_whitespace) {
            continue;
        }
        let candidate = Open {
            index,
            at,
            body_at: after + close + 1,
            attribute: attribute_of(inside),
        };
        // Earliest tag on the line wins; on a tie the longest name does, so
        // `<filter-trash>` is never read as `<filter-` plus junk.
        let better = match &best {
            None => true,
            Some(b) => {
                candidate.at < b.at || (candidate.at == b.at && candidate.body_at > b.body_at)
            }
        };
        if better {
            best = Some(candidate);
        }
    }
    best
}

/// `as="npm noise"` → `npm noise`. Single quotes work too; anything else is no
/// attribute at all rather than a parse error, because a lesson that mistypes
/// an attribute should still teach the lines.
fn attribute_of(inside: &str) -> String {
    let inside = inside.trim();
    let Some(rest) = inside.strip_prefix("as") else {
        return String::new();
    };
    let rest = rest
        .trim_start()
        .strip_prefix('=')
        .unwrap_or("")
        .trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
    match quote {
        Some(q) => rest[1..].split(q).next().unwrap_or("").trim().to_string(),
        None => rest.trim().to_string(),
    }
}

fn push(out: &mut Annotated, in_run: &mut bool, kind: Kind, summary: &str, line: &str) {
    // Whitespace carries no meaning downstream — a template is built from
    // `split_whitespace` — so a fragment cut out of an inline tag is stored the
    // same way a whole line is.
    let line = line.trim();
    if line.is_empty() {
        // A blank line is neither evidence nor a counter-example. Learning to
        // delete blank lines would be the most over-general rule possible, and
        // a blank line inside a banner must not split it into two runs either.
        return;
    }
    match kind {
        Kind::Trash => {
            out.trash.push(line.to_string());
            match out.runs.last_mut() {
                Some(run) if *in_run => run.push(line.to_string()),
                _ => out.runs.push(vec![line.to_string()]),
            }
            *in_run = true;
        }
        Kind::Fold => {
            // A folded run is trash with a caption. It is not a run for block
            // learning: the fold *is* the rule for it.
            if let Some(group) = out.folds.last_mut() {
                group.lines.push(line.to_string());
                if group.summary.is_empty() {
                    group.summary = summary.to_string();
                }
            }
            *in_run = false;
        }
        Kind::Only => {
            out.only.push(line.to_string());
            // Also a counter-example: nothing may learn to delete a line the
            // lesson just called the interesting one.
            out.keep.push(line.to_string());
            *in_run = false;
        }
        Kind::Undo => {
            *in_run = false;
            out.keep.push(line.to_string());
            // An instruction, not merely evidence: whatever removed this line
            // was wrong and gets retired.
            out.explicit_keep.push(line.to_string());
        }
        Kind::Plain => {
            *in_run = false;
            out.keep.push(line.to_string());
        }
    }
}

/// Does `text` contain any annotation at all?
///
/// Used to give a precise error instead of "learned 0 rules" when somebody
/// pipes raw output into `ttk learn` and forgets the tags.
pub fn has_markup(text: &str) -> bool {
    TAGS.iter().any(|t| text.contains(t.open))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_separates_trash_from_keep() {
        let a = parse(
            "added 412 packages\n\
             <filter-trash>\n\
             npm WARN deprecated glob@7.2.3\n\
             npm WARN deprecated inflight@1.0.6\n\
             </filter-trash>\n\
             found 0 vulnerabilities\n",
        );
        assert_eq!(a.trash.len(), 2);
        assert_eq!(
            a.keep,
            vec!["added 412 packages", "found 0 vulnerabilities"]
        );
        assert_eq!(a.blocks, 1);
        assert!(!a.unclosed);
    }

    #[test]
    fn inline_tags_work_on_one_line() {
        let a = parse("keep me <trash>drop me</trash> keep this too");
        assert_eq!(a.trash, vec!["drop me"]);
        assert_eq!(a.keep, vec!["keep me", "keep this too"]);
    }

    #[test]
    fn blank_lines_are_never_evidence() {
        let a = parse("<filter-trash>\n\n   \n</filter-trash>\n\n");
        assert!(a.is_empty());
        assert!(a.keep.is_empty());
        assert_eq!(a.blocks, 1);
    }

    #[test]
    fn an_unclosed_block_is_kept_but_flagged() {
        let a = parse("<filter-trash>\nnoise one\nnoise two\n");
        assert_eq!(a.trash.len(), 2);
        assert!(a.unclosed);
    }

    #[test]
    fn keep_tags_produce_counter_examples() {
        let a = parse(
            "plain line
<filter-keep>do not touch this</filter-keep>",
        );
        assert!(a.trash.is_empty());
        assert_eq!(a.keep, vec!["plain line", "do not touch this"]);
        assert_eq!(
            a.explicit_keep,
            vec!["do not touch this"],
            "only a tagged line is an instruction, not merely evidence"
        );
        assert_eq!(a.blocks, 0);
    }

    #[test]
    fn several_blocks_accumulate() {
        let a = parse("<trash>a</trash>\nkeep\n<trash>b</trash>");
        assert_eq!(a.trash, vec!["a", "b"]);
        assert_eq!(a.keep, vec!["keep"]);
        assert_eq!(a.blocks, 2);
    }

    #[test]
    fn an_unmarked_line_ends_a_run() {
        let a = parse(
            "<filter-trash>\none\ntwo\n</filter-trash>\n\
             something kept\n\
             <filter-trash>\nthree\n</filter-trash>\n",
        );
        assert_eq!(a.trash, vec!["one", "two", "three"]);
        assert_eq!(
            a.runs,
            vec![vec!["one", "two"], vec!["three"]],
            "contiguity is what block rules are built from"
        );
    }

    #[test]
    fn a_blank_line_inside_a_banner_does_not_split_the_run() {
        let a = parse("<filter-trash>\ntop\n\nbottom\n</filter-trash>\n");
        assert_eq!(a.runs, vec![vec!["top", "bottom"]]);
    }

    #[test]
    fn two_adjacent_trash_blocks_are_one_run() {
        let a = parse("<trash>a</trash>\n<trash>b</trash>");
        assert_eq!(
            a.runs,
            vec![vec!["a", "b"]],
            "nothing separates them, so nothing separates the run"
        );
    }

    #[test]
    fn runs_always_flatten_back_to_trash() {
        let a = parse("keep\n<trash>a</trash>\nkeep\n<trash>b</trash>\n<trash>c</trash>");
        let flat: Vec<String> = a.runs.iter().flatten().cloned().collect();
        assert_eq!(flat, a.trash);
    }

    #[test]
    fn markup_detection_is_independent_of_content() {
        assert!(has_markup("x <trash>y</trash>"));
        assert!(!has_markup("no tags here"));
    }

    #[test]
    fn plain_text_is_all_counter_examples() {
        let a = parse("one\ntwo\n");
        assert!(a.trash.is_empty());
        assert_eq!(a.keep.len(), 2);
    }
}
