//! `ttk install` — teach a coding agent to route its commands through `ttk`.
//!
//! The instruction file of every supported agent is *shared with the user*, so
//! this module never overwrites one. It maintains a single clearly marked
//! block, appends it if it is missing and replaces it in place if it is
//! already there. Running install twice changes nothing the second time.
//!
//! Writing outside the project (the user-global instruction files) always
//! requires an explicit confirmation or `--yes`.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use ttk_core::{Error, Result};

pub const BLOCK_BEGIN: &str =
    "<!-- BEGIN ThanosTokenKiller (ttk) — managed block, do not edit by hand -->";
pub const BLOCK_END: &str = "<!-- END ThanosTokenKiller (ttk) -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::ClaudeCode, Agent::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    /// File name used inside a project.
    pub fn project_file(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "CLAUDE.md",
            Agent::Codex => "AGENTS.md",
        }
    }

    /// User-global instruction file, if the agent has one.
    pub fn global_path(self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        Some(match self {
            Agent::ClaudeCode => home.join(".claude").join("CLAUDE.md"),
            Agent::Codex => home.join(".codex").join("AGENTS.md"),
        })
    }

    pub fn parse(s: &str) -> Result<Vec<Agent>> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => vec![Agent::ClaudeCode],
            "codex" => vec![Agent::Codex],
            "all" => Agent::ALL.to_vec(),
            other => {
                return Err(Error::Config(format!(
                    "unknown install target `{other}` (expected: claude, codex, all)"
                )));
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
    Both,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "project" | "repo" | "local" => Scope::Project,
            "global" | "user" => Scope::Global,
            "both" | "all" => Scope::Both,
            other => {
                return Err(Error::Config(format!(
                    "unknown install scope `{other}` (expected: project, global, both)"
                )));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub agent: Agent,
    pub path: PathBuf,
    pub global: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    BlockAdded,
    BlockUpdated,
    Unchanged,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::BlockAdded => "block added",
            Action::BlockUpdated => "block updated",
            Action::Unchanged => "already up to date",
        }
    }
}

/// Resolve which files to touch.
pub fn targets(agents: &[Agent], scope: Scope, project_root: &Path) -> Vec<Target> {
    let mut out = Vec::new();
    for &agent in agents {
        if matches!(scope, Scope::Project | Scope::Both) {
            out.push(Target {
                agent,
                path: project_root.join(agent.project_file()),
                global: false,
            });
        }
        if matches!(scope, Scope::Global | Scope::Both)
            && let Some(p) = agent.global_path()
        {
            out.push(Target {
                agent,
                path: p,
                global: true,
            });
        }
    }
    out
}

/// Insert or replace the managed block in `existing`.
///
/// Everything outside the markers is preserved byte for byte.
pub fn upsert_block(existing: &str, block: &str) -> String {
    upsert(existing, block, BLOCK_BEGIN, BLOCK_END)
}

/// Insert or replace a marked block, with caller supplied markers.
///
/// Shared with [`crate::path_env`], which maintains a `#`-commented block in a
/// shell profile with exactly the same guarantees.
pub fn upsert(existing: &str, block: &str, begin: &str, end: &str) -> String {
    let managed = format!("{begin}\n{}\n{end}\n", block.trim_end());

    if let Some(start) = existing.find(begin) {
        let after = &existing[start..];
        if let Some(rel_end) = after.find(end) {
            let stop = start + rel_end + end.len();
            let tail = existing[stop..]
                .strip_prefix('\n')
                .unwrap_or(&existing[stop..]);
            return format!("{}{managed}{tail}", &existing[..start]);
        }
        // Begin marker without an end marker: refuse to guess, append instead.
    }

    if existing.trim().is_empty() {
        return managed;
    }
    let separator = if existing.ends_with("\n\n") {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{existing}{separator}{managed}")
}

/// Write the guide into one target file.
pub fn apply(target: &Target, block: &str, dry_run: bool) -> Result<Action> {
    let existed = target.path.is_file();
    let existing = if existed {
        std::fs::read_to_string(&target.path)?
    } else {
        String::new()
    };
    let updated = upsert_block(&existing, block);

    let action = if !existed {
        Action::Created
    } else if existing == updated {
        Action::Unchanged
    } else if existing.contains(BLOCK_BEGIN) {
        Action::BlockUpdated
    } else {
        Action::BlockAdded
    };

    if dry_run || action == Action::Unchanged {
        return Ok(action);
    }

    if let Some(parent) = target.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a sibling temp file and rename, so an interrupted install can
    // never truncate a user's instruction file.
    let tmp = target.path.with_extension("ttk-tmp");
    std::fs::write(&tmp, updated.as_bytes())?;
    match std::fs::rename(&tmp, &target.path) {
        Ok(()) => {}
        Err(_) => {
            // Windows refuses to rename onto an existing file on some volumes.
            std::fs::write(&target.path, updated.as_bytes())?;
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(action)
}

// ---------------------------------------------------------------------------
// Interactive selection
// ---------------------------------------------------------------------------

/// Ask the user to pick one of `options`. Returns the chosen index.
pub fn choose(question: &str, options: &[&str]) -> Result<usize> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::other(
            "no terminal available for the interactive prompt — pass --target and --scope instead \
             (e.g. `ttk install --target all --scope project --yes`)",
        ));
    }
    loop {
        println!("\n{question}");
        for (i, o) in options.iter().enumerate() {
            println!("  {}) {o}", i + 1);
        }
        print!("select [1-{}]: ", options.len());
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Err(Error::other("input ended before a choice was made"));
        }
        match line.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= options.len() => return Ok(n - 1),
            _ => println!("please enter a number between 1 and {}", options.len()),
        }
    }
}

pub fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::other(
            "no terminal available to confirm — re-run with --yes if you are sure",
        ));
    }
    print!("{question} [y/N]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = "hello\nworld";

    #[test]
    fn creates_a_block_in_an_empty_file() {
        let out = upsert_block("", BLOCK);
        assert!(out.starts_with(BLOCK_BEGIN));
        assert!(out.trim_end().ends_with(BLOCK_END));
        assert!(out.contains("hello\nworld"));
    }

    #[test]
    fn appends_without_touching_existing_content() {
        let existing = "# My project rules\n\nAlways run tests.\n";
        let out = upsert_block(existing, BLOCK);
        assert!(out.starts_with(existing));
        assert!(out.contains(BLOCK_BEGIN));
    }

    #[test]
    fn is_idempotent() {
        let once = upsert_block("# rules\n", BLOCK);
        let twice = upsert_block(&once, BLOCK);
        assert_eq!(once, twice);
        assert_eq!(once.matches(BLOCK_BEGIN).count(), 1);
    }

    #[test]
    fn replaces_an_outdated_block_in_place() {
        let old = upsert_block("# rules\n", "old content");
        let new = upsert_block(&old, "new content");
        assert!(new.contains("new content"));
        assert!(!new.contains("old content"));
        assert!(new.starts_with("# rules\n"));
        assert_eq!(new.matches(BLOCK_BEGIN).count(), 1);
    }

    #[test]
    fn preserves_text_after_the_block() {
        let mut doc = upsert_block("# rules\n", "old");
        doc.push_str("\n## my own section\nkeep me\n");
        let out = upsert_block(&doc, "new");
        assert!(out.contains("## my own section"));
        assert!(out.contains("keep me"));
        assert!(out.contains("new"));
    }

    #[test]
    fn a_dangling_begin_marker_is_not_guessed_at() {
        let broken = format!("# rules\n{BLOCK_BEGIN}\nsomething truncated\n");
        let out = upsert_block(&broken, BLOCK);
        // The damaged text is kept; a fresh, complete block is appended.
        assert!(out.contains("something truncated"));
        assert_eq!(out.matches(BLOCK_END).count(), 1);
    }

    #[test]
    fn target_paths_are_agent_specific() {
        let root = Path::new("/repo");
        let t = targets(&Agent::ALL, Scope::Project, root);
        assert_eq!(t.len(), 2);
        assert!(t[0].path.ends_with("CLAUDE.md"));
        assert!(t[1].path.ends_with("AGENTS.md"));
        assert!(t.iter().all(|t| !t.global));
    }

    #[test]
    fn scope_both_covers_project_and_global() {
        let t = targets(&[Agent::ClaudeCode], Scope::Both, Path::new("/repo"));
        // The global entry only appears when a home directory exists.
        assert!(t.iter().any(|t| !t.global));
        if dirs::home_dir().is_some() {
            assert!(t.iter().any(|t| t.global));
        }
    }

    #[test]
    fn parsing_rejects_nonsense() {
        assert_eq!(Agent::parse("claude").unwrap(), vec![Agent::ClaudeCode]);
        assert_eq!(Agent::parse("ALL").unwrap().len(), 2);
        assert!(Agent::parse("emacs").is_err());
        assert_eq!(Scope::parse("global").unwrap(), Scope::Global);
        assert!(Scope::parse("everywhere").is_err());
    }

    #[test]
    fn apply_writes_and_is_idempotent_on_disk() {
        let dir = tempfile::tempdir().expect("tmp");
        let target = Target {
            agent: Agent::ClaudeCode,
            path: dir.path().join("CLAUDE.md"),
            global: false,
        };
        assert_eq!(
            apply(&target, BLOCK, false).expect("write"),
            Action::Created
        );
        assert_eq!(
            apply(&target, BLOCK, false).expect("write"),
            Action::Unchanged
        );
        let text = std::fs::read_to_string(&target.path).expect("read");
        assert_eq!(text.matches(BLOCK_BEGIN).count(), 1);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        let target = Target {
            agent: Agent::Codex,
            path: dir.path().join("AGENTS.md"),
            global: false,
        };
        assert_eq!(apply(&target, BLOCK, true).expect("dry"), Action::Created);
        assert!(!target.path.exists());
    }
}
