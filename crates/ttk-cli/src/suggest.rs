//! `ttk suggest` — drafting the lesson so a human only has to approve it.
//!
//! Learned filters work, and almost nobody teaches them. The tags are two
//! lines to type and the payoff is real, but it is one more thing to remember
//! at exactly the moment you were doing something else. That friction, not the
//! design, is what decides whether a project ends up with fifty rules or none.
//!
//! So this command does the noticing. It reads the originals ttk has already
//! stored — no new data, no new capture — and asks one question of them:
//!
//! > which line shapes keep coming back, run after run, and has anybody ever
//! > gone looking at them?
//!
//! A shape that appears in most runs of a command and that no rule covers is a
//! candidate. The output is a ready-made `<filter-trash>` document: read it,
//! delete anything you actually want, and pipe it into `ttk learn`.
//!
//! It **never writes a rule**. The whole safety argument of learned filters
//! rests on a person or an agent having looked at the lines, and a tool that
//! quietly decides what you do not need is precisely the failure this project
//! exists to avoid.

use std::collections::BTreeMap;

use ttk_core::Result;
use ttk_learn::{Rule, Scope, Template};
use ttk_store::Workspace;

/// How many recent sessions are scanned.
const SEARCH_SESSIONS: usize = 20;

/// A repeating line shape nothing removes yet.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub scope: Scope,
    /// Canonical template, for display.
    pub pattern: String,
    /// A real line that has this shape, which is what goes in the lesson.
    pub example: String,
    /// How many separate runs printed a line of this shape.
    pub runs: u64,
    /// How many lines of this shape were printed in total.
    pub lines: u64,
    /// Estimated tokens those lines cost.
    pub tokens: u64,
}

impl Suggestion {
    /// How many runs of this command were looked at, as a share.
    pub fn share(&self, of_runs: u64) -> f64 {
        if of_runs == 0 {
            return 0.0;
        }
        self.runs as f64 / of_runs as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct Suggestions {
    pub items: Vec<Suggestion>,
    /// Runs examined, per scope, so a share can be shown honestly.
    pub runs_by_scope: BTreeMap<String, u64>,
    pub events_scanned: u64,
}

/// Everything one line shape accumulated while scanning.
#[derive(Default)]
struct Tally {
    runs: u64,
    lines: u64,
    tokens: u64,
    example: String,
    /// Event ids already counted, so one run cannot inflate `runs`.
    last_event: String,
}

/// Scan stored originals for repeating noise no rule covers.
///
/// `min_runs` is the whole judgement: a shape seen once is not a pattern, and
/// pretending otherwise would fill the suggestion list with the contents of
/// whatever command happened to run last.
pub fn scan(
    workspace: &Workspace,
    rules: &[&Rule],
    min_runs: u64,
    only: Option<&Scope>,
) -> Result<Suggestions> {
    let mut out = Suggestions::default();
    // (scope, canonical template) → tally
    let mut tallies: BTreeMap<(String, String), Tally> = BTreeMap::new();
    let mut scopes: BTreeMap<String, Scope> = BTreeMap::new();

    for session in workspace
        .events()
        .sessions()?
        .into_iter()
        .take(SEARCH_SESSIONS)
    {
        for event in workspace.events().read_session(&session)? {
            let Some(command) = event.metadata.get("command").and_then(|v| v.as_str()) else {
                // Unattributed content has no scope, so a rule about it could
                // only ever be global. Suggesting that is not conservative.
                continue;
            };
            let scope = scope_of(command);
            if let Some(want) = only
                && *want != scope
            {
                continue;
            }
            let Some(capsule_id) = &event.capsule_id else {
                continue;
            };
            let Ok(text) =
                workspace
                    .capsules()
                    .render(capsule_id.as_str(), ttk_store::LEVEL_RAW, true)
            else {
                // An expired capsule is a normal outcome, not a failure.
                continue;
            };

            out.events_scanned += 1;
            let key = scope.to_string();
            *out.runs_by_scope.entry(key.clone()).or_insert(0) += 1;
            scopes.entry(key.clone()).or_insert_with(|| scope.clone());

            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                // Never suggest deleting evidence, and never suggest something
                // an existing rule already handles.
                if ttk_learn::rule::looks_critical(line) {
                    continue;
                }
                if rules.iter().any(|r| r.matches(line)) {
                    continue;
                }
                let Some(template) = Template::from_line(line) else {
                    continue;
                };
                let entry = tallies
                    .entry((key.clone(), template.canonical()))
                    .or_default();
                if entry.last_event != event.id.as_str() {
                    entry.runs += 1;
                    entry.last_event = event.id.to_string();
                }
                entry.lines += 1;
                entry.tokens += ttk_core::tokens::estimate(line).value;
                if entry.example.is_empty() {
                    entry.example = line.trim().to_string();
                }
            }
        }
    }

    for ((scope_key, pattern), tally) in tallies {
        if tally.runs < min_runs {
            continue;
        }
        out.items.push(Suggestion {
            scope: scopes.get(&scope_key).cloned().unwrap_or_default(),
            pattern,
            example: tally.example,
            runs: tally.runs,
            lines: tally.lines,
            tokens: tally.tokens,
        });
    }
    // Ranked by what removing it would actually be worth.
    out.items
        .sort_by_key(|s| (std::cmp::Reverse(s.tokens), s.pattern.clone()));
    Ok(out)
}

/// `"npm install --save"` → the scope a rule about it would have.
fn scope_of(command_line: &str) -> Scope {
    let (first, rest) = match command_line.strip_prefix('"') {
        Some(r) => r.split_once('"').unwrap_or((r, "")),
        None => command_line
            .split_once(char::is_whitespace)
            .unwrap_or((command_line, "")),
    };
    match rest.split_whitespace().find(|a| !a.starts_with('-')) {
        Some(sub) => Scope::command(first, sub),
        None => Scope::program(first),
    }
}

/// Render suggestions as a lesson somebody can read, edit and pipe back.
///
/// Deliberately the exact input format of `ttk learn`, with one example line
/// per shape, so approving the draft is a pipe and rejecting part of it is a
/// delete.
pub fn to_lesson(suggestions: &[Suggestion], scope: &Scope) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Draft lesson for `{scope}` — review it, delete what you want to keep,\n\
         # then:  ttk suggest --scope \"{scope}\" | ttk learn --scope \"{scope}\"\n\
         #\n\
         # Nothing here has been learned. These are line shapes that came back in\n\
         # most runs and that no rule covers; error-shaped lines were never\n\
         # considered.\n\n"
    ));
    out.push_str("<filter-trash>\n");
    for s in suggestions {
        out.push_str(&format!("{}\n", s.example));
    }
    out.push_str("</filter-trash>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_becomes_the_scope_a_rule_would_have() {
        assert_eq!(
            scope_of("npm install --save"),
            Scope::command("npm", "install")
        );
        assert_eq!(scope_of("pytest -q"), Scope::program("pytest"));
        assert_eq!(
            scope_of(r#""C:\Program Files\nodejs\NPM.EXE" ci"#),
            Scope::command("npm", "ci")
        );
    }

    #[test]
    fn a_share_is_reported_against_the_runs_that_were_looked_at() {
        let s = Suggestion {
            scope: Scope::program("npm"),
            pattern: "npm WARN deprecated {ver}".into(),
            example: "npm WARN deprecated a@1.0.0".into(),
            runs: 3,
            lines: 40,
            tokens: 400,
        };
        assert!((s.share(4) - 0.75).abs() < f64::EPSILON);
        assert_eq!(s.share(0), 0.0, "no runs is not a division by zero");
    }

    #[test]
    fn a_draft_lesson_is_valid_input_for_learn() {
        let items = vec![Suggestion {
            scope: Scope::program("npm"),
            pattern: "npm WARN deprecated {ver}".into(),
            example: "npm WARN deprecated a@1.0.0".into(),
            runs: 3,
            lines: 40,
            tokens: 400,
        }];
        let text = to_lesson(&items, &Scope::program("npm"));
        let parsed = ttk_learn::annotate::parse(&text);
        assert_eq!(parsed.trash, vec!["npm WARN deprecated a@1.0.0"]);
        assert!(
            text.contains("Nothing here has been learned"),
            "a draft has to say it is a draft: {text}"
        );
    }
}
