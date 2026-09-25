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
        let home = agent_home()?;
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
    /// A second copy of the block was taken out of this file.
    DuplicateRemoved,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::BlockAdded => "block added",
            Action::BlockUpdated => "block updated",
            Action::Unchanged => "already up to date",
            Action::DuplicateRemoved => "duplicate removed",
        }
    }
}

impl Agent {
    /// The instruction file in the home directory itself (`~/CLAUDE.md`).
    ///
    /// Claude Code reads every `CLAUDE.md` from the working directory upwards,
    /// so this file is read in *every* project below the home directory: it is
    /// global in effect, and a block there duplicates the one in the real
    /// global file.
    pub fn home_level_path(self) -> Option<PathBuf> {
        Some(agent_home()?.join(self.project_file()))
    }
}

/// The home directory the agents read their instruction files from.
///
/// `TTK_AGENT_HOME` replaces it, so the test suite never reads or writes the
/// developer's real `~/.claude/CLAUDE.md`.
fn agent_home() -> Option<PathBuf> {
    match std::env::var("TTK_AGENT_HOME") {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => dirs::home_dir(),
    }
}

/// Resolve which files to touch.
///
/// A "project" install whose project root is the home directory is really a
/// global one (see [`Agent::home_level_path`]), so it goes to the global file
/// instead, and no path is ever listed twice.
pub fn targets(agents: &[Agent], scope: Scope, project_root: &Path) -> Vec<Target> {
    let home = agent_home();
    let root_is_home = home.as_deref().is_some_and(|h| same_dir(h, project_root));
    let mut out: Vec<Target> = Vec::new();
    let mut push = |t: Target| {
        if !out.iter().any(|o| o.path == t.path) {
            out.push(t);
        }
    };
    for &agent in agents {
        let project = matches!(scope, Scope::Project | Scope::Both);
        let global = matches!(scope, Scope::Global | Scope::Both) || (project && root_is_home);
        if project && !root_is_home {
            push(Target {
                agent,
                path: project_root.join(agent.project_file()),
                global: false,
            });
        }
        if global && let Some(p) = agent.global_path() {
            push(Target {
                agent,
                path: p,
                global: true,
            });
        }
    }
    out
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Does this file already carry a ttk block (marked or not)?
pub fn has_block(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|t| t.contains(BLOCK_BEGIN) || legacy_heading(&t).is_some())
}

/// Was the block in this file written with `--compact`? Updates keep the form
/// the user chose.
pub fn has_compact_block(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains("**Teach the filter.**"))
}

/// Take the ttk block out of a file, leaving everything else. `true` when
/// there was one.
pub fn remove_block(path: &Path, dry_run: bool) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let (rest, found) = strip_blocks(&existing);
    if found.is_none() {
        return Ok(false);
    }
    if !dry_run {
        let rest = rest.trim_end();
        if rest.trim().is_empty() {
            // Nothing but the block was in it, so ttk created it: an empty
            // instruction file left behind would only be clutter.
            std::fs::remove_file(path)?;
        } else {
            write_atomically(path, &restore_newlines(&existing, &format!("{rest}\n")))?;
        }
    }
    Ok(true)
}

/// Insert or replace the managed block in `existing`.
///
/// Guarantees exactly one block afterwards, whatever state the file was in:
/// duplicate blocks, a block whose end marker was lost, and an unmarked copy
/// pasted in by hand are all replaced by the one current block, at the place
/// the first of them stood. Everything else is preserved, including Windows
/// line endings.
pub fn upsert_block(existing: &str, block: &str) -> String {
    let managed = format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}\n", block.trim_end());
    let (rest, at) = strip_blocks(existing);
    let text = match at {
        Some(at) => {
            let (before, after) = rest.split_at(at);
            let sep = if before.is_empty() || before.ends_with("\n\n") {
                ""
            } else if before.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            let after = after.trim_start_matches('\n');
            let gap = if after.is_empty() { "" } else { "\n" };
            format!("{before}{sep}{managed}{gap}{after}")
        }
        None => append(&rest, &managed),
    };
    restore_newlines(existing, &text)
}

/// Remove every ttk block from `existing` (normalised to `\n`), returning the
/// rest and the offset where the first block stood.
fn strip_blocks(existing: &str) -> (String, Option<usize>) {
    let mut text = existing.replace("\r\n", "\n");
    let mut first: Option<usize> = None;
    let note = |at: usize, first: &mut Option<usize>| {
        *first = Some(first.map_or(at, |f: usize| f.min(at)));
    };

    // 1. Complete marked blocks, however many there are.
    while let Some(start) = text.find(BLOCK_BEGIN) {
        let Some(rel_end) = text[start..].find(BLOCK_END) else {
            break;
        };
        let mut stop = start + rel_end + BLOCK_END.len();
        if text[stop..].starts_with('\n') {
            stop += 1;
        }
        text.replace_range(start..stop, "");
        note(start, &mut first);
    }
    // 2. A begin marker whose end marker was lost: drop the marker line, and
    //    let step 3 take the section that follows it.
    while let Some(start) = text.find(BLOCK_BEGIN) {
        let mut stop = start + BLOCK_BEGIN.len();
        if text[stop..].starts_with('\n') {
            stop += 1;
        }
        text.replace_range(start..stop, "");
        note(start, &mut first);
    }
    // 3. Unmarked copies of the section, from its heading to the next heading
    //    of the same or a higher level.
    while let Some((start, stop)) = legacy_heading(&text) {
        text.replace_range(start..stop, "");
        note(start, &mut first);
    }
    (text, first)
}

/// The span of an unmarked `## ThanosTokenKiller (`ttk`)` section.
fn legacy_heading(text: &str) -> Option<(usize, usize)> {
    const HEADING: &str = "## ThanosTokenKiller (`ttk`)";
    let start = if text.starts_with(HEADING) {
        0
    } else {
        text.find(&format!("\n{HEADING}"))? + 1
    };
    let body_start = start + HEADING.len();
    let mut offset = body_start;
    let mut in_fence = false;
    for line in text[body_start..].split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let ends_section = !in_fence
            && offset > body_start
            && (line.starts_with("# ") || line.starts_with("## ") || line.starts_with("<!--"));
        if ends_section {
            return Some((start, offset));
        }
        offset += line.len();
    }
    Some((start, text.len()))
}

fn append(existing: &str, managed: &str) -> String {
    if existing.trim().is_empty() {
        return managed.to_string();
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

/// Give `text` the line endings `original` used.
fn restore_newlines(original: &str, text: &str) -> String {
    if original.contains("\r\n") {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// Insert or replace a marked block, with caller supplied markers.
///
/// Shared with [`crate::path_env`], which maintains a `#`-commented block in a
/// shell profile with exactly the same guarantees. Only Unix has one.
#[cfg_attr(windows, allow(dead_code))]
pub fn upsert(existing: &str, block: &str, begin: &str, end: &str) -> String {
    let managed = format!("{begin}\n{}\n{end}\n", block.trim_end());

    if let Some(start) = existing.find(begin) {
        let after = &existing[start..];
        if let Some(rel_end) = after.find(end) {
            let stop = start + rel_end + end.len();
            let tail = existing[stop..]
                .strip_prefix('\n')
                .unwrap_or(&existing[stop..]);
            let head = &existing[..start];
            return format!("{head}{managed}{tail}");
        }
        // Begin marker without an end marker: refuse to guess, append instead.
    }
    append(existing, &managed)
}

/// Would this target give the agent a second copy of the block?
///
/// True for a project file when the agent's global file already carries the
/// block: the agent reads both, so the project copy is pure duplication.
pub fn redundant_with_global(target: &Target) -> bool {
    !target.global && target.agent.global_path().as_deref().is_some_and(has_block)
}

/// After writing a global file, remove the copy in the home directory's own
/// instruction file, which the agent would otherwise read a second time.
pub fn remove_home_duplicate(target: &Target, dry_run: bool) -> Result<Option<PathBuf>> {
    if !target.global {
        return Ok(None);
    }
    let Some(home_level) = target.agent.home_level_path() else {
        return Ok(None);
    };
    Ok(remove_block(&home_level, dry_run)?.then_some(home_level))
}

/// Bring every ttk block that is already installed up to date.
///
/// What an upgrade runs: the global instruction files are refreshed in the
/// form they were written in (full or compact), and a copy in the home
/// directory's own `CLAUDE.md` / `AGENTS.md`, which duplicates the global one,
/// is removed. Files without a block are left alone.
pub fn refresh_installed(dry_run: bool) -> Result<Vec<(PathBuf, Action)>> {
    let mut out = Vec::new();
    for agent in Agent::ALL {
        let global = agent.global_path();
        let home_level = agent.home_level_path();
        let global_has = global.as_deref().is_some_and(has_block);
        let home_has = home_level.as_deref().is_some_and(has_block);

        // A block only in the home-level file moves to the global file.
        let write_global = global_has || home_has;
        if write_global && let Some(path) = &global {
            let source = if global_has {
                path.clone()
            } else {
                home_level.clone().unwrap_or_default()
            };
            let block = crate::guide::agent_block_for(agent, has_compact_block(&source));
            let target = Target {
                agent,
                path: path.clone(),
                global: true,
            };
            out.push((path.clone(), apply(&target, &block, dry_run)?));
        }
        if home_has
            && let Some(path) = &home_level
            && remove_block(path, dry_run)?
        {
            out.push((path.clone(), Action::DuplicateRemoved));
        }
    }
    Ok(out)
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a sibling temp file and rename, so an interrupted install can
    // never truncate a user's instruction file.
    let tmp = path.with_extension("ttk-tmp");
    std::fs::write(&tmp, text.as_bytes())?;
    if std::fs::rename(&tmp, path).is_err() {
        // Windows refuses to rename onto an existing file on some volumes.
        std::fs::write(path, text.as_bytes())?;
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(())
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
    } else if existing.contains(BLOCK_BEGIN) || legacy_heading(&existing).is_some() {
        Action::BlockUpdated
    } else {
        Action::BlockAdded
    };

    if dry_run || action == Action::Unchanged {
        return Ok(action);
    }
    write_atomically(&target.path, &updated)?;
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
    fn duplicate_blocks_collapse_into_one_at_the_first_place() {
        let one = upsert_block("", "old one");
        let doc = format!("# mine\n\n{one}\n## middle\nkeep\n\n{one}\n## end\n");
        let out = upsert_block(&doc, "fresh");
        assert_eq!(out.matches(BLOCK_BEGIN).count(), 1, "{out}");
        assert_eq!(out.matches(BLOCK_END).count(), 1, "{out}");
        assert!(!out.contains("old one"), "{out}");
        assert!(out.contains("fresh"));
        // Where the first block stood, and nothing of the user's is lost.
        assert!(out.find("fresh") < out.find("## middle"), "{out}");
        assert!(out.contains("keep") && out.contains("## end") && out.starts_with("# mine"));
    }

    #[test]
    fn an_unmarked_pasted_copy_is_replaced_not_duplicated() {
        let doc = "# mine\n\n## ThanosTokenKiller (`ttk`)\n\nold text\n\n```bash\n## not a heading, a comment\n```\n\n### Rules\nold rules\n\n## Mine again\nkeep me\n";
        let out = upsert_block(doc, "## ThanosTokenKiller (`ttk`)\n\nnew text");
        assert_eq!(out.matches("## ThanosTokenKiller").count(), 1, "{out}");
        assert!(
            !out.contains("old text") && !out.contains("old rules"),
            "{out}"
        );
        assert!(out.contains("## Mine again\nkeep me"), "{out}");
        assert!(out.find("new text") < out.find("## Mine again"));
    }

    #[test]
    fn a_marker_without_its_end_is_repaired() {
        let doc = format!("# mine\n{BLOCK_BEGIN}\n## ThanosTokenKiller (`ttk`)\ntruncated\n");
        let out = upsert_block(&doc, "## ThanosTokenKiller (`ttk`)\nfresh");
        assert_eq!(out.matches(BLOCK_BEGIN).count(), 1, "{out}");
        assert!(!out.contains("truncated"), "{out}");
        assert!(out.starts_with("# mine\n"));
    }

    #[test]
    fn windows_line_endings_survive_an_update() {
        let doc = upsert_block("# mine\r\n", "old")
            .replace('\n', "\r\n")
            .replace("\r\r\n", "\r\n");
        let out = upsert_block(&doc, "line one\nline two");
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "only CRLF: {out:?}"
        );
        assert!(out.contains("line one\r\nline two"));
        assert_eq!(out.matches(BLOCK_BEGIN).count(), 1);
        assert_eq!(upsert_block(&out, "line one\nline two"), out, "idempotent");
    }

    #[test]
    fn removing_a_block_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, upsert_block("# mine\nkeep\n", "ttk stuff")).expect("write");
        assert!(has_block(&path));
        assert!(remove_block(&path, false).expect("remove"));
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text, "# mine\nkeep\n");
        assert!(!has_block(&path));
        assert!(!remove_block(&path, false).expect("again"));

        // A file that held nothing but the block goes away entirely.
        std::fs::write(&path, upsert_block("", "ttk stuff")).expect("write");
        assert!(remove_block(&path, false).expect("remove"));
        assert!(!path.exists());
    }

    #[test]
    fn a_project_install_in_the_home_directory_is_a_global_one() {
        let Some(home) = agent_home() else { return };
        let t = targets(&[Agent::ClaudeCode], Scope::Project, &home);
        assert_eq!(t.len(), 1);
        assert!(t[0].global, "{:?}", t[0]);
        let both = targets(&[Agent::ClaudeCode], Scope::Both, &home);
        assert_eq!(both.len(), 1, "no path twice");
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
