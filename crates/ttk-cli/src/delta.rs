//! Saying "the same as last time" instead of saying it all again.
//!
//! An agent runs `cargo test` eleven times in a row. Ten of those runs produce
//! byte-identical output, and every one of them is paid for again — not once,
//! but on every later request, because it stays in the conversation. This is
//! the single most repetitive thing a coding agent does, and no compiler helps
//! with it: a compiler shrinks one message, it cannot know the message was
//! already sent.
//!
//! Two candidates come from here:
//!
//! * [`repeat`] — the output is byte-identical to an earlier run.
//! * [`delta`] — the output differs, but mostly does not.
//!
//! # Why this is safe
//!
//! Both candidates would, naively, delete evidence: "same as before" contains
//! no stack trace. So neither of them is allowed to be only a pointer. Both
//! carry **every error-shaped line of the current output**, taken from the same
//! `critical_regions` pass the quality firewall uses to build its mandatory
//! invariants. A repeated *passing* test collapses to one line; a repeated
//! *failing* test still shows the failure, every time.
//!
//! That is not a special case bolted on — it is what makes these ordinary
//! candidates. The firewall reviews them exactly like a compiler's output, and
//! rejects them on the same terms: if a critical line went missing, or the
//! result is not actually smaller, the original stands.

use ttk_core::config::Config;
use ttk_core::event::TokenEvent;
use ttk_core::firewall::Candidate;
use ttk_core::invariants;
use ttk_store::Workspace;

/// How many recent sessions are searched for an earlier run of a command.
///
/// Bounded on purpose: a workspace with a thousand sessions must not turn every
/// command into a full scan, and an identical run from three weeks ago is not
/// in the conversation any more anyway.
const SEARCH_SESSIONS: usize = 3;

/// Oldest an earlier run may be and still be worth pointing at.
const MAX_AGE_MILLIS: u64 = 6 * 60 * 60 * 1000;

/// An earlier run of the same command.
pub struct Previous {
    /// `cap://…` of that run's stored original.
    pub reference: String,
    pub at_millis: u64,
    pub source_hash: String,
    /// The stored original, if it could still be read.
    pub content: Option<String>,
    /// How many earlier runs of this command produced this same output.
    pub repeats: u64,
    /// The learned rules that fired on that run ([`FILTER_KEY`]); empty when
    /// none did. A run is only comparable with one filtered the same way.
    pub filter_key: String,
}

/// Event metadata key: the learned rules that fired on a run.
pub const FILTER_KEY: &str = "filter_key";

impl Previous {
    fn age_millis(&self, now: u64) -> u64 {
        now.saturating_sub(self.at_millis)
    }
}

/// Find the most recent earlier run of `command` in this workspace.
///
/// Returns `None` for content with no command attached: without knowing what
/// produced two pieces of text, "the same as last time" is a claim we cannot
/// make.
pub fn previous_run(
    workspace: &Workspace,
    command: Option<&str>,
    now_millis: u64,
) -> Option<Previous> {
    let command = command?;
    let sessions = workspace.events().sessions().ok()?;

    let mut newest: Option<TokenEvent> = None;
    let mut repeats = 0u64;
    for session in sessions.into_iter().take(SEARCH_SESSIONS) {
        let events = workspace.events().read_session(&session).ok()?;
        for event in events.into_iter().rev() {
            if event.metadata.get("command").and_then(|v| v.as_str()) != Some(command) {
                continue;
            }
            if now_millis.saturating_sub(event.timestamp_millis) > MAX_AGE_MILLIS {
                continue;
            }
            match &newest {
                None => newest = Some(event),
                Some(first) => {
                    // Everything after the newest only contributes the repeat
                    // count, which is what makes "3rd identical run" honest.
                    if first.source_hash == event.source_hash {
                        repeats += 1;
                    }
                }
            }
        }
        if newest.is_some() {
            break;
        }
    }

    let event = newest?;
    let capsule_id = event.capsule_id.as_ref()?;
    let capsule = workspace.capsules().get(capsule_id.as_str()).ok()?;
    let content = workspace
        .capsules()
        .render(capsule_id.as_str(), ttk_store::LEVEL_RAW, true)
        .ok();

    Some(Previous {
        reference: capsule.reference(),
        at_millis: event.timestamp_millis,
        source_hash: event.source_hash.clone(),
        content,
        repeats,
        filter_key: event
            .metadata
            .get(FILTER_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// `2m ago`, `3h ago`, `just now`.
fn ago(millis: u64) -> String {
    let secs = millis / 1000;
    match secs {
        0..=30 => "just now".to_string(),
        31..=90 => "a minute ago".to_string(),
        91..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

/// The error-shaped lines of `text`, which every candidate here has to carry.
fn critical(text: &str) -> String {
    invariants::critical_regions(text)
}

/// A candidate for "byte-identical to an earlier run".
///
/// `None` when there is no earlier run, or the output is not identical to it.
pub fn repeat(
    current: &str,
    source_hash: &str,
    previous: &Previous,
    now: u64,
) -> Option<Candidate> {
    if previous.source_hash != source_hash {
        return None;
    }
    let mut out = format!(
        "[repeat] identical to {} ({}",
        previous.reference,
        ago(previous.age_millis(now))
    );
    if previous.repeats > 0 {
        // "run 3 of the same output" is worth a token: it tells an agent it is
        // in a loop that is not converging.
        out.push_str(&format!(
            ", {} identical run(s) before that",
            previous.repeats
        ));
    }
    out.push_str(")\n");

    // Everything that explains a failure travels with the pointer, always.
    let keep = critical(current);
    if !keep.is_empty() {
        out.push_str(&keep);
    }

    Some(
        Candidate::new("repeat.suppress", 1, out)
            .min_gain(0.0)
            .note(format!("identical to {}", previous.reference)),
    )
}

/// A candidate for "mostly the same as an earlier run".
///
/// Emits the lines that are new plus a count of what is unchanged, and — as
/// always — every error-shaped line of the current output.
pub fn delta(current: &str, previous: &Previous, now: u64, config: &Config) -> Option<Candidate> {
    let old = previous.content.as_deref()?;
    if old == current {
        return None;
    }
    // Comparing two enormous inputs line by line is not worth the memory; the
    // compilers handle that case better anyway.
    if current.len() as u64 > config.limits.max_parse_bytes
        || old.len() as u64 > config.limits.max_parse_bytes
    {
        return None;
    }

    let before: std::collections::HashSet<&str> = old.lines().map(str::trim_end).collect();
    let mut added: Vec<&str> = Vec::new();
    let mut unchanged = 0u64;
    for line in current.lines() {
        if before.contains(line.trim_end()) {
            unchanged += 1;
        } else {
            added.push(line);
        }
    }
    // Nothing in common is not a delta, it is a different output.
    if unchanged == 0 || added.len() as u64 >= unchanged {
        return None;
    }
    let removed = old
        .lines()
        .filter(|l| !current.lines().any(|c| c.trim_end() == l.trim_end()))
        .count();

    let mut out = format!(
        "[delta] vs {} ({})\nunchanged={unchanged} added={} removed={removed}\n",
        previous.reference,
        ago(previous.age_millis(now)),
        added.len()
    );
    for line in &added {
        out.push_str(line);
        out.push('\n');
    }

    // The unchanged part may still contain the failure an agent is chasing, so
    // it comes along even though it is, by definition, not new.
    let keep = critical(current);
    for line in keep.lines() {
        if !added.contains(&line) {
            out.push_str(line);
            out.push('\n');
        }
    }

    Some(
        Candidate::new("delta.lines", 1, out)
            .min_gain(0.0)
            .note(format!(
                "{unchanged} line(s) unchanged since {}",
                previous.reference
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;

    fn prev(content: &str, at: u64) -> Previous {
        Previous {
            reference: "cap://a7f3c".to_string(),
            at_millis: at,
            source_hash: ttk_core::hash_string(content),
            content: Some(content.to_string()),
            repeats: 0,
            filter_key: String::new(),
        }
    }

    const PASSING: &str = "running 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... ok\n\
                           test result: ok. 3 passed; 0 failed\n";

    const FAILING: &str = "running 3 tests\ntest a ... ok\ntest b ... FAILED\n\
                           ---- b stdout ----\nassertion failed: 1 == 2\n\
                           test result: FAILED. 2 passed; 1 failed\n";

    #[test]
    fn an_identical_passing_run_collapses_to_a_pointer() {
        let p = prev(PASSING, NOW - 120_000);
        let c = repeat(PASSING, &ttk_core::hash_string(PASSING), &p, NOW).expect("repeat");
        assert!(c.output.starts_with("[repeat] identical to cap://a7f3c"));
        assert!(c.output.contains("2m ago"), "{}", c.output);
        assert!(
            c.output.lines().count() <= 2,
            "nothing to preserve, so nothing is: {}",
            c.output
        );
        assert!(c.output.len() < PASSING.len());
    }

    /// The point of the whole design: repeating does not mean forgetting.
    #[test]
    fn an_identical_failing_run_still_shows_the_failure() {
        let p = prev(FAILING, NOW - 120_000);
        let c = repeat(FAILING, &ttk_core::hash_string(FAILING), &p, NOW).expect("repeat");
        assert!(c.output.contains("[repeat]"));
        assert!(
            c.output.contains("assertion failed: 1 == 2"),
            "{}",
            c.output
        );
        assert!(c.output.contains("FAILED"), "{}", c.output);
    }

    #[test]
    fn different_output_is_not_a_repeat() {
        let p = prev(PASSING, NOW - 1000);
        assert!(repeat(FAILING, &ttk_core::hash_string(FAILING), &p, NOW).is_none());
    }

    #[test]
    fn a_repeated_repeat_says_how_deep_the_loop_is() {
        let mut p = prev(PASSING, NOW - 1000);
        p.repeats = 4;
        let c = repeat(PASSING, &ttk_core::hash_string(PASSING), &p, NOW).expect("repeat");
        assert!(
            c.output.contains("4 identical run(s) before"),
            "{}",
            c.output
        );
    }

    #[test]
    fn a_delta_carries_only_what_changed() {
        let config = Config::default();
        let old = "line one\nline two\nline three\nline four\nline five\n";
        let new = "line one\nline two\nline three\nline four\nline six\n";
        let c = delta(new, &prev(old, NOW - 60_000), NOW, &config).expect("delta");
        assert!(c.output.contains("unchanged=4"), "{}", c.output);
        assert!(c.output.contains("added=1"), "{}", c.output);
        assert!(c.output.contains("removed=1"), "{}", c.output);
        assert!(c.output.contains("line six"));
        assert!(!c.output.contains("line three"), "{}", c.output);
    }

    #[test]
    fn a_delta_never_drops_a_failure_just_because_it_is_old_news() {
        let config = Config::default();
        let old = format!("{FAILING}extra line one\nextra line two\nextra line three\n");
        let new = format!("{FAILING}extra line one\nextra line two\nnew line here\n");
        let c = delta(&new, &prev(&old, NOW - 60_000), NOW, &config).expect("delta");
        assert!(c.output.contains("new line here"));
        assert!(
            c.output.contains("assertion failed: 1 == 2"),
            "unchanged, but still the thing being debugged: {}",
            c.output
        );
    }

    #[test]
    fn output_with_nothing_in_common_is_not_a_delta() {
        let config = Config::default();
        let p = prev("aaa\nbbb\nccc\n", NOW - 1000);
        assert!(delta("xxx\nyyy\nzzz\n", &p, NOW, &config).is_none());
    }

    #[test]
    fn a_delta_that_would_be_most_of_the_output_is_refused() {
        let config = Config::default();
        let p = prev("a\nb\nc\nd\n", NOW - 1000);
        // Three new lines against one kept: not worth calling a delta.
        assert!(delta("a\nx\ny\nz\n", &p, NOW, &config).is_none());
    }

    #[test]
    fn identical_content_is_not_a_delta() {
        let config = Config::default();
        assert!(delta(PASSING, &prev(PASSING, NOW - 1000), NOW, &config).is_none());
    }

    #[test]
    fn ages_read_the_way_people_say_them() {
        assert_eq!(ago(5_000), "just now");
        assert_eq!(ago(60_000), "a minute ago");
        assert_eq!(ago(600_000), "10m ago");
        assert_eq!(ago(7_200_000), "2h ago");
    }
}
