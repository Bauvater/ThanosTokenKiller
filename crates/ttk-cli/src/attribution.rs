//! Who a ledger record belongs to: session, agent, subagent, model.
//!
//! Every number ttk reports can be broken down by these, so they are taken
//! from the most reliable source available:
//!
//! 1. A hook's stdin (`session_id`, `agent_id`, `agent_type`) — exact.
//! 2. The environment the agent gives its child processes
//!    (`CLAUDE_CODE_SESSION_ID`, `CLAUDECODE`, `CODEX_*`) — exact for the
//!    session, silent about subagents.
//! 3. `TTK_SESSION` / `TTK_AGENT` / `TTK_MODEL`, for anything else.
//!
//! The model is usually unknown at capture time; reports join it from the
//! measured provider usage of the same session.

use std::path::{Path, PathBuf};

use ttk_store::{RecordKind, RunRecord};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribution {
    pub session: Option<String>,
    pub agent: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub via: &'static str,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Attribution {
    /// Attribution from the environment of this process.
    pub fn from_env(via: &'static str) -> Self {
        let agent = env("TTK_AGENT").or_else(|| {
            if env("CLAUDECODE").is_some() || env("CLAUDE_CODE_SESSION_ID").is_some() {
                Some("claude-code".to_string())
            } else if std::env::vars().any(|(k, _)| k.starts_with("CODEX_")) {
                Some("codex".to_string())
            } else {
                None
            }
        });
        Self {
            session: env("CLAUDE_CODE_SESSION_ID")
                .or_else(|| env("CODEX_SESSION_ID"))
                .or_else(|| env("TTK_SESSION")),
            agent: agent.or_else(|| Some("shell".to_string())),
            agent_id: env("TTK_AGENT_ID"),
            agent_type: env("TTK_AGENT_TYPE"),
            model: env("TTK_MODEL").or_else(|| env("ANTHROPIC_MODEL")),
            via,
        }
    }

    /// Fill a record's attribution fields.
    pub fn apply(&self, r: &mut RunRecord) {
        r.session = self.session.clone();
        r.agent = self.agent.clone();
        r.agent_id = self.agent_id.clone();
        r.agent_type = self.agent_type.clone();
        r.model = self.model.clone();
        r.via = Some(self.via.to_string());
    }
}

/// The project a record belongs to: the nearest project root above `cwd`.
pub fn project_root(cwd: &Path) -> PathBuf {
    ttk_core::config::find_project_root(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// What one delivery cost and saved, ready for the ledger.
pub struct Delivery<'a> {
    pub id: Option<String>,
    pub program: Option<String>,
    pub tokens_before: u64,
    /// Tokens of what the agent actually received on stdout.
    pub tokens_after: u64,
    /// Tokens ttk printed in addition (summary lines on stderr, hints).
    pub overhead: u64,
    /// What the overhead was: `output` (summary lines), `hint`.
    pub cause: &'static str,
    pub filtered_lines: u64,
    pub kind: RecordKind,
    pub cwd: &'a Path,
}

/// Append one record to the global ledger. Best effort: a statistic that
/// cannot be written must never fail the command a user ran.
pub fn record(attr: &Attribution, d: Delivery<'_>) {
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return;
    };
    let root = project_root(d.cwd);
    let mut r = RunRecord::new(&root, d.tokens_before, d.tokens_after);
    r.id = d.id;
    r.program = d.program.filter(|p| !p.is_empty());
    r.overhead = d.overhead;
    if d.overhead > 0 {
        r.cause = Some(d.cause.to_string());
    }
    r.filtered_lines = d.filtered_lines;
    r.kind = d.kind;
    attr.apply(&mut r);
    if let Err(e) = ledger.append(&r) {
        eprintln!("ttk: global usage not recorded: {e}");
    }
}

/// Book tokens ttk put into the context on its own.
pub fn record_overhead(
    attr: &Attribution,
    cwd: &Path,
    tokens: u64,
    cause: &str,
    id: Option<String>,
) {
    if tokens == 0 {
        return;
    }
    let Some(ledger) = ttk_store::UsageLedger::open() else {
        return;
    };
    let root = project_root(cwd);
    let mut r = RunRecord::overhead(&root, tokens, cause);
    r.id = id;
    attr.apply(&mut r);
    if let Err(e) = ledger.append(&r) {
        eprintln!("ttk: global usage not recorded: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_fills_every_field() {
        let a = Attribution {
            session: Some("s".into()),
            agent: Some("claude-code".into()),
            agent_id: Some("a1".into()),
            agent_type: Some("Explore".into()),
            model: Some("m".into()),
            via: "hook",
        };
        let mut r = RunRecord::new(Path::new("/p"), 10, 1);
        a.apply(&mut r);
        assert_eq!(r.agent_bucket(), "subagent:Explore");
        assert_eq!(r.via.as_deref(), Some("hook"));
        assert_eq!(r.model.as_deref(), Some("m"));
    }
}
