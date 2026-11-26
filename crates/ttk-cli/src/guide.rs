//! The text that `ttk help` prints and `ttk install` writes into an agent's
//! instruction file.
//!
//! Both come from here so the CLI, the generated agent rules and the README
//! can never drift apart. The savings table holds **measured** numbers and is
//! never rounded up; the method is documented in the README.

use crate::install::Agent;

/// Measured with the built-in estimator over the golden fixtures in
/// `crates/ttk-compilers/tests/golden.rs`, run end to end through the CLI.
/// One row = one real input, not an average over a corpus.
pub const SAVINGS_TABLE: &str = "\
| input                                    | compiler        | mode     | before | after | saved |
|------------------------------------------|-----------------|----------|-------:|------:|------:|
| `cargo test` of this repo (149 tests)     | test.cargo      | safe     |  3,605 |    62 |  98%  |
| application log, 501 lines                | log.cluster     | balanced | 14,536 |   165 |  99%  |
| JSON array, 200 objects                   | json.summary    | balanced |  4,211 |   112 |  97%  |
| pytest, 818 tests, 2 failures             | test.pytest     | safe     |    759 |   180 |  76%  |
| `git status`, 4 changed files             | git.status      | safe     |    244 |   113 |  54%  |
| `cargo test`, small fixture               | test.cargo      | safe     |    282 |   167 |  41%  |
| `go test`, 3 tests, 1 failure             | test.go         | safe     |    122 |    75 |  39%  |
| `git diff`, 2 files                       | git.diff        | safe     |    330 |   204 |  38%  |
| `git log`, 2 commits                      | git.log         | safe     |    170 |   108 |  36%  |
| jest, 813 tests, 1 failure                | test.jest       | safe     |    157 |   112 |  29%  |
| rustc errors, 2 diagnostics               | build.cargo     | safe     |    171 |   126 |  26%  |
| JSON array, 40 objects, pretty printed    | json.structural | safe     |  1,439 | 1,235 |  14%  |
| `rg` output, 12 matches in 4 files        | shell.grep      | safe     |    257 |   238 |   7%  |
| application log, 501 lines                | log.cluster     | safe     | 14,536 | 14,536|   0%  |";

pub const SAVINGS_NOTE: &str = "\
Savings scale with volume: the compilers keep a constant amount of signal, so
the more repetitive output you feed them, the higher the percentage. Small
fixtures land at 7-40%, real test runs and logs at 76-99%.

The last row is not a mistake: log clustering is lossy, so `safe` mode refuses
it and returns the original untouched. That is the intended behaviour.

All numbers use the built-in heuristic estimator (`~`), measured end to end
through the CLI. They are single measurements, not a benchmark suite.";

/// The central rule file, `ttk.md`, exactly as shipped in the repository.
///
/// It is written next to every instruction file `ttk install` touches, and
/// `ttk guide` prints it. One file, one source of truth: the CLAUDE.md block,
/// the AGENTS.md block and the hook's injected context are all derived from it.
pub const TTK_MD: &str = include_str!("../../../ttk.md");

/// File name of the rule file beside an instruction file.
pub const TTK_MD_FILE: &str = "ttk.md";

/// The core rules: the part of `ttk.md` between its `ttk:core` markers.
pub fn core_rules() -> &'static str {
    let begin = "<!-- ttk:core -->";
    let end = "<!-- /ttk:core -->";
    match (TTK_MD.find(begin), TTK_MD.find(end)) {
        (Some(b), Some(e)) if b < e => TTK_MD[b + begin.len()..e].trim(),
        _ => TTK_MD,
    }
}

/// What the hook injects at session start, after compaction and into every
/// subagent. Short on purpose: it is paid for in every one of those contexts.
pub fn agent_context() -> String {
    format!(
        "{}\nFull rules: `ttk.md` in the project or `ttk guide` (read it before teaching filters).",
        core_rules()
    )
}

/// The compact block: only the core rules, plus where the rest lives.
pub fn agent_block_compact(agent: Agent) -> String {
    match agent {
        Agent::ClaudeCode => agent_block(agent),
        Agent::Codex => format!(
            "## ThanosTokenKiller (`ttk`) — mandatory\n\n\
             Every agent and subagent MUST read `{TTK_MD_FILE}` (next to this file) \
             completely and follow it. Prefix shell commands with long output with \
             `ttk run --`.\n\n{}\n",
            core_rules()
        ),
    }
}

/// The block for one agent, full or compact.
pub fn agent_block_for(agent: Agent, compact: bool) -> String {
    if compact {
        agent_block_compact(agent)
    } else {
        agent_block(agent)
    }
}

/// The markdown block written into `CLAUDE.md` / `AGENTS.md`.
///
/// Claude Code expands `@file` imports, so its block is two sentences and the
/// import: the rules load with the instruction file, in every session and every
/// subagent, without being copied into it. Codex has no imports, so it gets the
/// rule file inline.
pub fn agent_block(agent: Agent) -> String {
    match agent {
        Agent::ClaudeCode => format!(
            "## ThanosTokenKiller (`ttk`) — mandatory\n\n\
             **Every agent and every subagent MUST read `{TTK_MD_FILE}` completely and follow \
             all of its instructions.** It is imported here so it is always loaded:\n\n\
             @{TTK_MD_FILE}\n"
        ),
        Agent::Codex => format!(
            "## ThanosTokenKiller (`ttk`) — mandatory\n\n\
             Every agent and subagent MUST follow these rules (also in `{TTK_MD_FILE}`). \
             Prefix shell commands with long output with `ttk run --`.\n\n{}\n",
            TTK_MD.trim()
        ),
    }
}

/// The cheat sheet printed by `ttk help`.
pub fn cheat_sheet() -> String {
    format!(
        r#"ThanosTokenKiller {version} — the only Claude Code token killer you'll ever need!

  Capture verbose tool output, compile it into a compact form, verify that
  nothing critical was lost, and keep the byte exact original nearby.

SETUP
  ttk setup                     the whole walkthrough: workspace, agent, PATH,
                                and a first run so you can see it work
  ttk init                      create .ttk/ and a project config
  ttk install                   teach Claude Code / Codex to use ttk, and put ttk
                                on your user PATH (interactive)
  ttk install --no-path         …without touching PATH
  ttk install --target all --scope project --yes     non-interactive
  ttk install --compact         write the ~200 token instruction block instead
                                of the ~800 token one (it is in every request)
  ttk doctor                    check the installation, the workspace and PATH
  ttk config [--paths]          show the effective configuration and its layers

EVERYDAY USE
  ttk run -- <command>          run a command, get compiled output, keep its exit code
  ttk run --stream -- <cmd>     …and also stream the raw output while it runs
  <something> | ttk compile     compile stdin
  ttk compile --file <path>     compile a file
  ttk read <path>               read a file; an unchanged re-read costs a pointer
  ttk read --outline <path>     its declarations and line numbers instead
  ttk read --lines A:B <path>   a numbered slice

  Running the same command twice does not cost twice. Identical output
  collapses to `[repeat] identical to cap://…`, mostly-identical output to a
  `[delta]` of what changed — and either way every error line comes with it.

TEACHING THE FILTER
  Nobody hand-writes filters here. Mark the junk with <filter-trash> tags and
  feed it back; everything you leave unmarked becomes a counter-example, so an
  over-general rule is refused instead of learned. Repeated lines become a line
  rule; a banner that no single line describes becomes one block rule for the
  whole run.

  <filter-trash>…</filter-trash>          delete these lines
  <filter-fold as="what it was">…</…>     replace the run with that one line
  <filter-only>…</filter-only>            these are the ONLY lines worth keeping
  <filter-keep>…</filter-keep>            this was removed and should not be

  ttk suggest                   read what ttk already stored and draft a lesson
  ttk suggest --lesson | ttk learn        …after you have read the draft
  ttk learn --last              learn from the newest command, scoped to it
  ttk learn --capsule <cap>     …from a specific capsule
  ttk learn --dry-run           show the rules, write nothing
  ttk learn --global            apply to every command, not just this one
  ttk learn --user              store in the global folder: every project
  ttk rules                     what has been learned, ranked by tokens saved
  ttk rules show <id>           one rule in full
  ttk rules disable|enable|forget <id>
  ttk rules prune               drop rules that have never fired
  ttk rules export|import       move rules between machines
  ttk filter --explain          dry run the rules over stdin, change nothing

GETTING THE FULL DATA BACK
  ttk retrieve <capsule>              level 2 by default
  ttk retrieve <capsule> --level 4    the byte exact original
  ttk retrieve <capsule> --lines A:B  a numbered slice
  ttk search <capsule> <text>         search inside the original
  ttk raw <capsule>                   raw bytes to stdout

WHAT DID IT SAVE
  ttk gain                      everything ttk has saved, in every project
  ttk gain --project            …only the project you are in
  ttk gain --days 30            …with a longer daily chart
  ttk gain --session latest     one session of this workspace in detail
  ttk stats --global            the same total, with the per-project table
  ttk stats [--sessions N]      lifetime totals for this workspace
  ttk usage path|compact|forget maintain the global ledger
  ttk inspect <event>           one event
  ttk explain <event>           what a compiler did, and why it was accepted
  ttk replay [--session ID]     re-run today's compilers over stored originals

THE GLOBAL FOLDER
  ttk global                    where it is and what is in it
  ttk global --open             open it in the file manager
  ttk global --init             create it (the installer already does)

MAINTENANCE
  ttk capsule list|stats|gc|delete <id>

GLOBAL FLAGS
  --mode <MODE>   observe | safe | balanced | maximum | debug | forensic | offline
                  observe changes nothing, safe (default) refuses lossy steps,
                  balanced enables log clustering / JSON summaries / truncation
  --json          machine readable output where a command supports it
  --color <WHEN>  auto (default) | always | never

ENVIRONMENT
  TTK_HOME              use this directory as the workspace
  TTK_GLOBAL_HOME       move the global folder (filters, ledger, user config)
  TTK_USAGE=0           do not record anything in the global ledger
  TTK_SESSION           share one session id across several invocations
  TTK_MODE              default mode
  TTK_TELEMETRY=0       disable the local event log (nothing is ever sent anywhere)
  TTK_NO_PATH_SETUP=1   make `ttk install` never touch PATH
  TTK_ASCII=1           plain ASCII glyphs instead of box drawing characters
  NO_COLOR=1            no colour, ever (CLICOLOR_FORCE=1 forces it back on)

LEARNED FILTERS
  Rules live in .ttk/learned-filters.json — plain JSON, meant to be committed,
  so a project's accumulated knowledge travels with the repository. A rule is
  a token template, never a regex: literal words must match exactly and only
  volatile values ({{n}} {{ver}} {{hex}} {{t}} {{size}} {{path}} {{url}} {{uuid}}) are
  wildcards. A block rule is the same thing over several lines and removes the
  run as a whole or not at all; `ttk rules` shows its height in the `run`
  column. Error-shaped lines are never filtered — one anywhere in a run saves
  the whole block — and the quality firewall reviews a filtered result exactly
  like a compiler's output. See docs/learned-filters.md.

PATH SETUP
  `ttk install` appends the binary's directory to the user PATH. On Windows it
  edits HKCU\Environment through the registry API — never `setx`, which
  truncates any PATH longer than 1024 characters and would silently drop
  entries. On Unix it appends one marked block to your shell profile. Both are
  additive and idempotent; open a new terminal afterwards.

WHAT IT SAVES TODAY

{SAVINGS_TABLE}

{SAVINGS_NOTE}

THE GLOBAL FOLDER AND LEDGER
  One folder outside every project (%APPDATA%\ttk on Windows, ~/.config/ttk
  elsewhere) holds what applies everywhere:

    filters/        every *.json rule file here filters every project;
                    `ttk learn --user` writes filters/learned-filters.json
    usage.jsonl     one line per `ttk run`, in any repository
    config.toml     optional user configuration

  The ledger is how `ttk gain` answers "how much has this saved me" without
  you hunting through twenty projects. It holds local paths, it never leaves
  the machine, it folds itself into rollups when it grows, and
  `ttk usage forget --all` empties it.

  `ttk <command> --help` documents a single command.
  https://github.com/Bauvater/ThanosTokenKiller
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

/// What a bare `ttk` and `ttk help` print: the dozen commands that matter,
/// on one screen. `ttk help --all` has the rest.
pub fn overview() -> String {
    use crate::ui;

    let groups: [(&str, &[(&str, &str)]); 3] = [
        (
            "get started",
            &[
                ("ttk setup", "guided setup: workspace, agent, PATH"),
                (
                    "ttk run -- <command>",
                    "run a command, get the compact version",
                ),
                ("ttk gain", "how many tokens you have saved, in total"),
            ],
        ),
        (
            "every day",
            &[
                (
                    "ttk read <file>",
                    "read a file; --outline for just its structure",
                ),
                (
                    "ttk retrieve <capsule>",
                    "the full original, whenever you need it",
                ),
                (
                    "ttk learn --last",
                    "teach the filter which lines were noise",
                ),
                ("ttk rules", "what has been learned, and what it saved"),
            ],
        ),
        (
            "everywhere",
            &[
                ("ttk global", "the global folder: filters for every project"),
                ("ttk stats --global", "savings per project"),
                ("ttk doctor", "check the installation"),
            ],
        ),
    ];

    let mut out = ui::banner("the token killer for coding agents");
    out.push_str("\n\n");
    out.push_str(&format!(
        "  {}\n  {}\n",
        ui::paint(
            ui::HEAD,
            "Shrinks what your coding agent reads — tests, builds, git, logs —"
        ),
        ui::paint(
            ui::DIM,
            "and keeps every byte of the original one command away."
        )
    ));
    for (title, rows) in groups {
        out.push_str(&format!("{}\n", ui::heading(title)));
        for (cmd, what) in rows {
            let pad = 26usize.saturating_sub(cmd.chars().count());
            out.push_str(&format!(
                "  {}{}{}\n",
                ui::paint(ui::CODE, cmd),
                " ".repeat(pad),
                what
            ));
        }
    }
    out.push_str(&format!(
        "\n  {}  {}   {}  {}\n",
        ui::paint(ui::CODE, "ttk help --all"),
        ui::paint(ui::DIM, "every command"),
        ui::paint(ui::CODE, "ttk <command> --help"),
        ui::paint(ui::DIM, "one command in detail"),
    ));
    out
}

/// Colour a plain reference text such as [`cheat_sheet`] for a terminal.
///
/// The plain text stays the single source (tests, `--json`, the README read
/// it), so styling is derived from its layout instead of kept in a second
/// copy: an all-caps line is a section, an indented `ttk …` line is a command
/// with its description after the first wide gap.
pub fn styled(text: &str) -> String {
    use crate::ui;

    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            out.push_str(&ui::banner("reference"));
            out.push('\n');
            continue;
        }
        let trimmed = line.trim();
        let is_section = !line.starts_with(' ')
            && trimmed.len() > 2
            && trimmed
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == ' ' || c == '\'');
        if is_section {
            // The plain text already has the blank line a heading brings.
            out.push_str(ui::heading(trimmed).trim_start_matches('\n'));
            out.push('\n');
            continue;
        }
        let command_like = trimmed.starts_with("ttk ")
            || trimmed.starts_with("<something> |")
            || trimmed.starts_with('<') && trimmed.contains("</");
        if line.starts_with("  ") && command_like {
            let indent = line.len() - line.trim_start().len();
            let body = &line[indent..];
            match body.find("  ") {
                Some(gap) => {
                    let (cmd, rest) = body.split_at(gap);
                    let desc = rest.trim_start();
                    let spaces = rest.len() - desc.len();
                    out.push_str(&format!(
                        "{}{}{}{}\n",
                        " ".repeat(indent),
                        ui::paint(ui::CODE, cmd),
                        " ".repeat(spaces),
                        ui::paint(ui::DIM, desc)
                    ));
                }
                None => out.push_str(&format!(
                    "{}{}\n",
                    " ".repeat(indent),
                    ui::paint(ui::CODE, body)
                )),
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overview_fits_a_screen_and_names_the_essentials() {
        let text = overview();
        assert!(text.lines().count() < 30, "{}", text.lines().count());
        for needle in [
            "ttk run -- <command>",
            "ttk gain",
            "ttk global",
            "ttk help --all",
        ] {
            assert!(text.contains(needle), "missing `{needle}`");
        }
    }

    #[test]
    fn styling_keeps_every_word_of_the_reference() {
        let plain = cheat_sheet();
        let styled = styled(&plain);
        // Without a terminal the escapes are stripped later; the words are
        // all there already.
        for needle in [
            "ttk retrieve <capsule> --level 4",
            "WHAT IT SAVES TODAY",
            "ttk gain",
        ] {
            assert!(styled.contains(needle), "missing `{needle}`");
        }
    }

    #[test]
    fn the_claude_block_is_mandatory_and_imports_the_rule_file() {
        let text = agent_block(Agent::ClaudeCode);
        assert!(text.contains("MUST read `ttk.md` completely"));
        assert!(text.contains("\n@ttk.md\n"), "{text}");
        assert!(text.contains("subagent"));
        // The block itself is small: the rules live in ttk.md.
        assert!(text.len() < 400, "{}", text.len());
    }

    #[test]
    fn the_rule_file_teaches_retrieval_filters_and_fallback() {
        for needle in [
            "ttk retrieve",
            "--level 4",
            "ttk search",
            "_not_shown",
            "<secret:",
            "<filter-trash>",
            "<filter-keep>",
            "ttk learn --last",
            "counter-example",
            "ttk rules audit",
            "ttk rules restore",
            "--when success",
            "subagent",
        ] {
            assert!(TTK_MD.contains(needle), "ttk.md is missing `{needle}`");
        }
    }

    #[test]
    fn the_core_rules_are_a_small_part_of_the_rule_file() {
        let core = core_rules();
        assert!(core.contains("cap://"));
        assert!(core.contains("ttk retrieve"));
        assert!(!core.contains("ttk:core"), "markers are stripped");
        assert!(core.len() * 3 < TTK_MD.len());
        assert!(agent_context().contains("ttk guide"));
    }

    #[test]
    fn codex_gets_the_rules_inline_because_it_has_no_imports() {
        let full = agent_block(Agent::Codex);
        assert!(full.contains(TTK_MD.trim()));
        let compact = agent_block_compact(Agent::Codex);
        assert!(compact.contains(core_rules()));
        assert!(compact.len() * 2 < full.len());
        assert_eq!(agent_block_for(Agent::Codex, true), compact);
        assert_eq!(agent_block_for(Agent::Codex, false), full);
    }

    #[test]
    fn the_cheat_sheet_still_teaches_the_learning_loop() {
        let text = cheat_sheet();
        assert!(text.contains("<filter-trash>"));
        assert!(text.contains("ttk learn"));
        assert!(text.contains("ttk rules"));
        assert!(text.contains(SAVINGS_TABLE));
        assert!(text.contains("not a benchmark suite"));
    }
    #[test]
    fn the_cheat_sheet_lists_every_command_group() {
        let text = cheat_sheet();
        for needle in [
            "ttk init",
            "ttk install",
            "ttk doctor",
            "ttk run",
            "ttk compile",
            "ttk retrieve",
            "ttk search",
            "ttk raw",
            "ttk stats",
            "ttk gain",
            "ttk inspect",
            "ttk explain",
            "ttk replay",
            "ttk capsule",
            "ttk learn",
            "ttk rules",
            "ttk filter",
            "ttk suggest",
            "ttk read",
            "ttk setup",
            "ttk usage",
            "ttk stats --global",
        ] {
            assert!(text.contains(needle), "cheat sheet is missing `{needle}`");
        }
    }
}
