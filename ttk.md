# ThanosTokenKiller (ttk) — rules for every agent and subagent

ttk shrinks tool output before it reaches your context and keeps the byte-exact
original locally. Read this file completely and follow it. These rules apply to
the main agent and to every subagent.

<!-- ttk:core -->
**ttk is active.** Shell output is compiled by ttk automatically (hook) or via
`ttk run -- <cmd>`. Rules:
1. A `raw=cap://…` line means the full original is stored. If the compact view
   is not enough, fetch more instead of re-running: `ttk retrieve <cap> --lines A:B`,
   `ttk search <cap> "text"`, `ttk retrieve <cap> --level 4` (all).
2. `[repeat]`/`[delta]` = same as an earlier run; errors are always repeated.
3. Fields ending in `_not_shown` say exactly what was left out.
4. When output contained noise you will see again, teach a filter (rule 6 in
   ttk.md); improve an existing rule rather than adding a near-duplicate.
5. Never filter errors, warnings, test results, stack traces or exit codes. If
   unsure, keep the line — or read the original.
6. `<secret:…>` placeholders are redacted credentials: never unmask them.
<!-- /ttk:core -->

## 1. Running commands

- With the Claude Code hook installed, every Bash/PowerShell call is compiled
  automatically — run commands normally. Without it, prefix long-output
  commands with `ttk run --` (tests, builds, `git`, logs, package managers,
  `grep`/`rg`). Never wrap interactive commands or anything needing a TTY.
- `ttk run` forwards the exit code unchanged, so `&&` chains keep working.
- Files: `ttk read --outline <file>` for declarations, `ttk read --lines 40:80 <file>`
  for a slice. Re-reading an unchanged file costs a pointer.
- Small outputs (under ~200 tokens) are passed through untouched on purpose:
  compiling them would cost more than it saves.

## 2. Reading compiled output

```text
[test:pytest] status=fail
failed=2 passed=812 skipped=4 duration=71.20s
@fail#1 test=tests/auth/test_expiry.py::test_token_expiry at=tests/auth/test_expiry.py:87
  assert 200 == 401
raw=cap://cap_01KZ47VX…
```

Header, counters, one `@section` per finding, a pointer to the original.

## 3. Falling back to the original

The compact view is a proposal reviewed by ttk's quality firewall: if any error
line, count or path would be lost, ttk delivers the unfiltered original instead.
If you still suspect something is missing, **do not re-run** the command — use
`ttk retrieve` / `ttk search` on the capsule. Re-running costs the whole output again.

## 4. Filters you are expected to create

You are responsible for keeping recurring noise out of the context:

1. After a command whose output had lines you will never need (progress bars,
   banners, deprecation spam, download logs), teach a filter right away:
   ```bash
   ttk learn --last <<'EOF'
   <filter-trash>
   npm WARN deprecated inflight@1.0.6: This module is not supported
   </filter-trash>
   added 412 packages in 9s
   EOF
   ```
   Paste back the **whole** output; unmarked lines are counter-examples that
   stop a rule from over-matching.
2. Other tags: `<filter-fold as="npm: 40 deprecation warnings">…</filter-fold>`
   (one summary line instead), `<filter-only>…</filter-only>` (only these lines
   matter), `<filter-keep>…</filter-keep>` (retire a rule that removed this).
3. Conditional filters: `ttk learn --last --when success` applies only when the
   command succeeded — use it for noise you want to see when it fails.
4. Before teaching, check what exists: `ttk rules` (ranked by tokens saved) and
   `ttk suggest` (drafts a lesson from repeating shapes). Improve or widen an
   existing rule instead of adding a near-duplicate.
5. A filter costs tokens to teach. Only teach noise that will recur; ttk tracks
   each rule's net saving (saved − teaching cost) and flags negative ones.
6. A hint like `ttk: … little was filtered` is ttk asking you to do step 1.

## 5. Maintenance

- `ttk rules audit` — duplicate, subsumed, stale, never-hit and negative rules.
- `ttk rules history` / `ttk rules restore <version>` — every change is versioned.
- `ttk gain` — savings; `ttk dashboard` — full statistics in the browser.
- `ttk explain <event>` — what a compiler did and why.

## 6. Hard rules

- Errors, warnings, failures, stack traces, test results and exit codes are
  never noise. ttk refuses to learn rules for them; do not try `--force`.
- Never unmask `<secret:…>` placeholders.
- Do not disable ttk to "see everything"; retrieve the capsule instead.
