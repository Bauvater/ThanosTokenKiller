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

/// The teaching loop, written once and reused by every surface that explains
/// it. Wording here ends up in an agent's instruction file, so it is phrased as
/// an instruction rather than as a description.
pub const LEARNING_BLOCK: &str = r#"### Teach it what is worthless

ttk does not ship a fixed list of filters. **You** write them, by marking the
lines you never want to see again:

```bash
ttk run -- npm install            # you see forty deprecation warnings
ttk learn --last <<'EOF'
<filter-trash>
npm WARN deprecated inflight@1.0.6: This module is not supported
npm WARN deprecated glob@7.2.3: Glob versions prior to v9 are unsupported
</filter-trash>
added 412 packages in 9s
EOF
```

From now on those lines are removed from every `npm install` in this project,
before you ever see them, and the rule is stored in
`.ttk/learned-filters.json` for everyone on the team.

Rules for teaching:

1. **Paste back the whole output**, not only the junk. Everything you leave
   unmarked becomes a counter-example, and that is what stops a rule from
   growing teeth. A lesson with no unmarked text is a lesson with no brakes.
2. **Mark noise, never evidence.** Errors, failures, exit codes, stack traces
   and line references are refused outright — that is deliberate, not a bug.
3. **One command, one lesson.** `--last` takes the scope from the command that
   produced the output, so a rule about `npm install` never fires on `cargo`.
   Use `--global` only for noise that genuinely appears everywhere.
4. **If a rule removes something useful**, mark that line with
   `<filter-keep>…</filter-keep>` and teach again: the offending rule retires.
5. **Mark whole banners too.** Output that no single line describes — a boxed
   header, an ASCII logo — is learned as one *block* rule that removes the run
   as a whole. Just mark all of its lines together; nothing extra to type.
6. **Three more tags, when deleting is the wrong answer:**
   `<filter-fold as="npm: 40 deprecation warnings">…</filter-fold>` keeps a one
   line summary instead of nothing; `<filter-only>…</filter-only>` says *these
   are the only lines worth keeping* for this command, which is far cheaper to
   teach when output is 99% noise.
7. **`ttk suggest` does the noticing for you.** It reads what ttk already
   stored and drafts a lesson from the shapes that keep coming back. Read the
   draft, delete anything you want to keep, then pipe it into `ttk learn`.
8. `ttk rules` shows what has been learned and what each rule has saved;
   `ttk filter --explain` dry runs the rules over text without changing
   anything."#;

/// The compact block: the same instructions, an eighth of the tokens.
///
/// The full block is around 800 tokens, and it is in **every request** an agent
/// makes for the rest of the session. A tool whose job is to remove tokens
/// should be embarrassed about that, and on a long session the full block can
/// cost more than it saves on a short one. This is what survives when only the
/// load bearing sentences are kept.
pub fn agent_block_compact(agent: Agent) -> String {
    let run_hint = match agent {
        Agent::ClaudeCode => "Bash tool calls",
        Agent::Codex => "shell commands",
    };
    format!(
        r#"## ThanosTokenKiller (`ttk`)

Prefix {run_hint} with `ttk run --` for anything long: tests, builds, `git`,
logs, package managers, `grep`. The exit code passes through unchanged. Pipe
into `ttk compile`; read files with `ttk read --outline <path>`.

You get compact output plus `raw=cap://…`. That is the byte exact original:
`ttk retrieve <cap> --level 4`, `--lines A:B`, or `ttk search <cap> <text>`.
Never re-run an expensive command to see more of it. Any `*_not_shown` field
says exactly how much was left out. `<secret:…>` is a redacted credential —
work with the placeholder.

**Teach the filter.** Paste output back with junk wrapped in
`<filter-trash>…</filter-trash>` and run `ttk learn --last`. Those lines never
reach you again. Include the *whole* output: what you leave unmarked is the
counter-example that stops a rule over-matching. Errors are refused on purpose.
`<filter-fold as="what it was">…</filter-fold>` keeps a one line summary
instead; `<filter-only>…</filter-only>` says these are the only lines worth
keeping. Wrong rule? Mark the line `<filter-keep>` and teach again.

`ttk suggest` drafts a lesson from what it has already seen. `ttk rules` shows
what has been learned. `ttk gain` shows the running total.
"#
    )
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
pub fn agent_block(agent: Agent) -> String {
    let run_hint = match agent {
        Agent::ClaudeCode => "When you use the Bash tool, prefix the command with `ttk run --`.",
        Agent::Codex => "When you run a shell command, prefix it with `ttk run --`.",
    };

    format!(
        r#"## ThanosTokenKiller (`ttk`)

This project uses **ThanosTokenKiller** to shrink verbose tool output before it
reaches you. {run_hint}

```bash
ttk run -- cargo test          # instead of: cargo test
ttk run -- pytest -q           # instead of: pytest -q
ttk run -- git diff            # instead of: git diff
kubectl logs deploy/api | ttk compile
ttk read --outline src/main.rs # a file's declarations instead of the file
ttk read --lines 40:80 src/main.rs
```

Reading the same unchanged file twice costs a pointer the second time, and so
does running the same command twice: identical output collapses to
`[repeat] identical to cap://…`, and mostly-identical output to a `[delta]` of
what changed. Every error line travels with both, so a repeated *failure* still
shows the failure.

`ttk run` forwards the command's exit code unchanged, so `&&` chains and
scripts keep working. Compile a file or a pipe with `ttk compile`.

### What you get back

A compact Token IR document instead of raw output:

```text
[test:pytest] status=fail
failed=2 passed=812 skipped=4 duration=71.20s
@fail#1 test=tests/auth/test_expiry.py::test_token_expiry at=tests/auth/test_expiry.py:87 error=AssertionError
  assert 200 == 401
raw=cap://cap_01KZ47VXAHH06PDQ4YJ559M002
```

Read it as: header line, then counters, then one `@section` per finding, then a
pointer to the full original.

### Nothing is lost — retrieve it when you need it

The `raw=cap://…` line is a **capsule**: the byte exact original is stored
locally. Whenever the compiled view is not enough, fetch more instead of
re-running the command:

```bash
ttk retrieve <capsule> --level 4        # the complete original
ttk retrieve <capsule> --lines 100:180  # a numbered slice
ttk search   <capsule> "timeout"        # grep inside the original
```

Any field ending in `_not_shown` (`lines_not_shown`, `files_not_shown`, …)
tells you exactly how much was left out.

### Rules

1. Prefer `ttk run --` for commands whose output is long: tests, builds, `git`,
   logs, package managers, `grep`/`rg`.
2. Do **not** wrap interactive commands, editors, or anything that needs a TTY.
3. If output looks truncated or you need context around a hit, use
   `ttk retrieve` / `ttk search` — never re-run an expensive command just to
   see more of it.
4. `<secret:…>` placeholders mean a credential was detected and kept local.
   Never try to unmask one; work with the placeholder.
5. `ttk stats` shows this workspace, `ttk stats --global` shows every project
   at once; `ttk explain <event>` shows what a compiler did and why.

{LEARNING_BLOCK}

### What it saves today

{SAVINGS_TABLE}

{SAVINGS_NOTE}

The table above is what the built-in compilers do on their own. Learned filters
are additive and project specific, so they have no table: `ttk rules` reports
what yours have actually saved.
"#
    )
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
    fn the_block_names_the_agents_own_workflow() {
        assert!(agent_block(Agent::ClaudeCode).contains("Bash tool"));
        assert!(agent_block(Agent::Codex).contains("shell command"));
    }

    #[test]
    fn the_block_teaches_retrieval_not_just_compression() {
        let text = agent_block(Agent::ClaudeCode);
        assert!(text.contains("ttk retrieve"));
        assert!(text.contains("--level 4"));
        assert!(text.contains("ttk search"));
        assert!(text.contains("_not_shown"));
        assert!(text.contains("<secret:"));
    }

    #[test]
    fn the_savings_table_is_shared_everywhere() {
        assert!(agent_block(Agent::Codex).contains(SAVINGS_TABLE));
        assert!(cheat_sheet().contains(SAVINGS_TABLE));
        // The honest caveat must travel with the numbers.
        assert!(agent_block(Agent::Codex).contains("not a benchmark suite"));
        assert!(cheat_sheet().contains("not a benchmark suite"));
    }

    /// The compact block exists to be small. If it stops being much smaller
    /// than the full one, it has stopped being worth having.
    #[test]
    fn the_compact_block_is_a_fraction_of_the_full_one() {
        for agent in Agent::ALL {
            let full = agent_block(agent);
            let small = agent_block_compact(agent);
            assert!(
                small.len() * 3 < full.len(),
                "{} vs {} characters",
                small.len(),
                full.len()
            );
            // …and still teaches every load bearing thing.
            for needle in [
                "ttk run --",
                "ttk retrieve",
                "<filter-trash>",
                "ttk learn --last",
                "counter-example",
                "cap://",
            ] {
                assert!(
                    small.contains(needle),
                    "the compact block dropped `{needle}`"
                );
            }
        }
        assert_eq!(
            agent_block_for(Agent::Codex, false),
            agent_block(Agent::Codex)
        );
        assert_eq!(
            agent_block_for(Agent::Codex, true),
            agent_block_compact(Agent::Codex)
        );
    }

    #[test]
    fn the_learning_loop_is_taught_everywhere_it_matters() {
        for text in [
            agent_block(Agent::ClaudeCode),
            agent_block(Agent::Codex),
            cheat_sheet(),
        ] {
            assert!(
                text.contains("<filter-trash>"),
                "the tag itself must appear"
            );
            assert!(text.contains("ttk learn"));
            assert!(text.contains("ttk rules"));
        }
        // The single most important instruction: unmarked text is evidence.
        assert!(LEARNING_BLOCK.contains("counter-example"));
        assert!(LEARNING_BLOCK.contains("<filter-keep>"));
        assert!(agent_block(Agent::ClaudeCode).contains(LEARNING_BLOCK));
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
