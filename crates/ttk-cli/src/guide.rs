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
```

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
5. `ttk stats` shows how much has been saved so far; `ttk explain <event>`
   shows what a compiler did and why.

### What it saves today

{SAVINGS_TABLE}

{SAVINGS_NOTE}
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
  ttk init                      create .ttk/ and a project config
  ttk install                   teach Claude Code / Codex to use ttk, and put ttk
                                on your user PATH (interactive)
  ttk install --no-path         …without touching PATH
  ttk install --target all --scope project --yes     non-interactive
  ttk doctor                    check the installation, the workspace and PATH
  ttk config [--paths]          show the effective configuration and its layers

EVERYDAY USE
  ttk run -- <command>          run a command, get compiled output, keep its exit code
  ttk run --stream -- <cmd>     …and also stream the raw output while it runs
  <something> | ttk compile     compile stdin
  ttk compile --file <path>     compile a file

GETTING THE FULL DATA BACK
  ttk retrieve <capsule>              level 2 by default
  ttk retrieve <capsule> --level 4    the byte exact original
  ttk retrieve <capsule> --lines A:B  a numbered slice
  ttk search <capsule> <text>         search inside the original
  ttk raw <capsule>                   raw bytes to stdout

WHAT DID IT SAVE
  ttk stats [--sessions N]      lifetime totals: tokens, commands, transformers
  ttk gain [--session ID]       one session in detail
  ttk inspect <event>           one event
  ttk explain <event>           what a compiler did, and why it was accepted
  ttk replay [--session ID]     re-run today's compilers over stored originals

MAINTENANCE
  ttk capsule list|stats|gc|delete <id>

GLOBAL FLAGS
  --mode <MODE>   observe | safe | balanced | maximum | debug | forensic | offline
                  observe changes nothing, safe (default) refuses lossy steps,
                  balanced enables log clustering / JSON summaries / truncation
  --json          machine readable output where a command supports it

ENVIRONMENT
  TTK_HOME              use this directory as the workspace
  TTK_SESSION           share one session id across several invocations
  TTK_MODE              default mode
  TTK_TELEMETRY=0       disable the local event log (nothing is ever sent anywhere)
  TTK_NO_PATH_SETUP=1   make `ttk install` never touch PATH

PATH SETUP
  `ttk install` appends the binary's directory to the user PATH. On Windows it
  edits HKCU\Environment through the registry API — never `setx`, which
  truncates any PATH longer than 1024 characters and would silently drop
  entries. On Unix it appends one marked block to your shell profile. Both are
  additive and idempotent; open a new terminal afterwards.

WHAT IT SAVES TODAY

{SAVINGS_TABLE}

{SAVINGS_NOTE}

  `ttk <command> --help` documents a single command.
  https://github.com/Bauvater/ThanosTokenKiller
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ] {
            assert!(text.contains(needle), "cheat sheet is missing `{needle}`");
        }
    }
}
