# Changelog

## 0.2.1 — 2026-11-26

The first published release of the 0.2 line (the `v0.2.0` tag was never
released: its Linux build failed).

### Fixed

- **Learned rules never fired for quoted commands** (pre-existing, Linux CI
  red since 0.1): the scope of a lesson was read from the stored command line
  by splitting on whitespace, so `sh -c "cat out.txt"` was scoped to `"cat`
  while the running command reports `cat out.txt`. Stored command lines are
  now split quote-aware, shared by `ttk learn` and `ttk suggest`.
- **`shell.grep` silently dropped lines** it could not parse (context lines
  from `grep -A`, matches without a path) and files beyond its cap. They are
  now kept in an `other` section, or reported as `files_not_shown`.
- `ttk.md` is embedded with normalised line endings, so installs stay
  idempotent regardless of Git's CRLF conversion.
- Clippy on Rust 1.100.

### Installer

- An upgrade also registers the Claude Code hooks (pointed at the newly
  installed binary); `ttk dashboard` is on the closing card.
- Uninstalling removes ttk's hooks, instruction blocks and `ttk.md`, so no
  hook is left pointing at a deleted binary.

### CI

- Test failures are raised as annotations, readable without the full log.

## 0.2.0 — 2026-11-25

A ground-up rework of enforcement, token accounting, the filter system and the
reports. Existing ledgers, rule files and capsules keep working unchanged.

### Why the global token count was unreliable (root causes)

| # | Cause | Effect | Fix |
|---|---|---|---|
| 1 | Nothing forced output through ttk; agents had to remember `ttk run --` | most commands were never counted ("today: 1 run") | Claude Code `PostToolUse` hook compiles and counts every Bash/PowerShell output |
| 2 | `compact()` / `forget()` rewrote the ledger with no lock while others appended | appends landing between read and rename were lost — under parallel sessions | every ledger writer holds an OS file lock (released automatically if a process dies) |
| 3 | records had no id | retried writes and nested `ttk` (`ttk run -- sh -c "… \| ttk compile"`) counted the same output twice | records carry an id (`tool_use_id` / event id), aggregation dedupes; a nested `ttk` passes through (`TTK_NESTED`) |
| 4 | `ttk read --outline` / `--lines` recorded the full file as "after" | savings under-reported | the delivered view is what is recorded |
| 5 | every invocation minted a new random session id | no per-session/agent/model numbers possible | attribution from hook input (`session_id`, `agent_id`, `agent_type`) or `CLAUDE_CODE_SESSION_ID`; model joined from measured usage |
| 6 | ttk's own tokens were never booked | gross savings presented as if free | summary lines, injected rules, hints, lessons and retrievals are booked as overhead; **net = saved − overhead** |
| 7 | parallel rule-hit updates did load→modify→save without a lock | lost filter statistics | rule files are updated under a lock |
| 8 | the capsule index admitted one process; the first one held it during the whole command | parallel `ttk run` failed | the workspace is opened only after the command finished, and waits for the lock |

### Added

- **`ttk hook`** for Claude Code: `PostToolUse` (compile output before the model
  sees it), `SessionStart` incl. `compact` and `SubagentStart` (inject the core
  rules), `Stop`/`SubagentStop`/`SessionEnd` (import measured usage). Never
  fails the agent; errors go to `<global>/hook.log`.
- **`ttk install`** writes `ttk.md`, a mandatory `CLAUDE.md` block that imports
  it (`@ttk.md`, ≈80 tokens instead of ≈1 800) and registers the hooks without
  touching other hooks; `--remove-hooks`, `--no-hooks`. Warns about other hooks
  that rewrite shell commands (e.g. rtk).
- **`ttk.md`** — the central rule file for every agent and subagent; `ttk guide`.
- **Ledger v2**: record ids, kinds (`run`, `passthrough`, `overhead`), overhead
  causes, session / agent / subagent / model attribution. Old records read as before.
- **Measured usage**: `ttk usage sync` and the hooks import `message.usage`
  from Claude Code transcripts, one record per response (`message.id`), read
  incrementally; 4.8 GB of transcripts import in ~30 s, later syncs only read
  what is new. Shown next to — never added to — ttk's estimates.
- **`ttk dashboard`**: a self-contained offline HTML page (overview, sessions,
  projects, commands, models & agents, filters, settings), light and dark.
- **Filters**: versioning and restore (`ttk rules history|restore`), validation of
  new rules against stored outputs (withdrawn if they would match error lines),
  `ttk learn --when success|failure`, `ttk rules audit [--fix]` (duplicate,
  subsumed, stale, never-hit, net-negative, overbroad), per-rule teaching cost and
  net saving, hint to teach a filter when a large output was barely reduced.
- `[hooks]` configuration section (`min_tokens`, `min_saving`, `hints`, …).

### Changed

- `ttk gain` leads with the **net** saving and shows overhead, passthrough share
  and measured usage.
- The CLAUDE.md block no longer carries the savings table and full reference.

### Measured

Ten real outputs from this repository through `ttk hook` (release build,
isolated ledger): before ~33 688 tokens, after ~24 306, **gross 27.8 %**,
overhead 568 (335 for the injected rules, 233 for one hint), **net ~8 814**.
Test runs shrank 97–99 %, `git log` 50–93 %; `git diff`, `cargo build -v`,
`grep` and `ls` were left unchanged by the firewall — that is where learned
filters have to help.

### Known limitations

- Savings are estimates by construction (tokens that never reached a model
  cannot be measured). Claude has no public offline tokenizer, so ttk uses a
  calibrated heuristic and labels it `~`; measured provider usage is reported
  separately.
- The transcript format is internal to Claude Code; the importer reads it
  defensively and skips what it does not understand.
- Claude Code runs all matching hooks in parallel. Another hook that also
  rewrites Bash commands or output (e.g. `rtk hook claude`) stacks with ttk's;
  `ttk install` warns about it.
- The hook does not know a command's exit code, so `--when` rules only fire
  for `ttk run`, which does.
- `path_env::tests::environment_change_is_broadcast_without_hanging` depends
  on the desktop session and can time out under heavy load.
