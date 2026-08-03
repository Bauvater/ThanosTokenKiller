//! Golden tests for the specialized compilers.
//!
//! Every case asserts the four properties that matter:
//! 1. the compiler produces a candidate,
//! 2. the output is valid Token IR (or valid JSON),
//! 3. the facts that must survive are literally present,
//! 4. the result is smaller and deterministic.

use ttk_compilers::{CommandContext, CompileInput, compile};
use ttk_core::config::{Config, Mode};
use ttk_core::content;
use ttk_core::firewall::{Candidate, Firewall};
use ttk_core::ir;
use ttk_core::tokens;

fn cfg(mode: Mode) -> Config {
    Config {
        mode,
        ..Config::default()
    }
}

fn cmd(argv: &[&str], exit: i32) -> CommandContext {
    CommandContext {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: "/repo".to_string(),
        exit_code: Some(exit),
        signal: None,
        duration_ms: 71_200,
        stdout_bytes: 0,
        stderr_bytes: 0,
    }
}

/// Compile and assert the universal properties. Returns the candidate.
fn compile_ok(content: &str, ctx: Option<&CommandContext>, config: &Config) -> Candidate {
    let detection = content::detect(content, None);
    let mut input = CompileInput::new(content, config).content_type(detection.content_type);
    if let Some(c) = ctx {
        input = input.command(c);
    }
    let candidate = compile(&input).expect("a compiler must accept this input");

    // Determinism.
    let again = compile(&input).expect("second run");
    assert_eq!(
        candidate.output, again.output,
        "compiler is not deterministic"
    );

    candidate
}

fn assert_ir(candidate: &Candidate) -> ir::IrDoc {
    ir::parse(&candidate.output).unwrap_or_else(|e| {
        panic!(
            "output is not valid Token IR: {e}\n---\n{}",
            candidate.output
        )
    })
}

fn assert_shrinks(original: &str, candidate: &Candidate) {
    let before = tokens::estimate(original).value;
    let after = tokens::estimate(&candidate.output).value;
    assert!(
        after < before,
        "no reduction: {before} -> {after}\n{}",
        candidate.output
    );
}

fn assert_contains_all(candidate: &Candidate, needles: &[&str]) {
    for n in needles {
        assert!(
            candidate.output.contains(n),
            "missing `{n}` in output:\n{}",
            candidate.output
        );
    }
}

/// The firewall must accept the candidate in the given mode.
fn assert_firewall_accepts(original: &str, candidate: Candidate, config: &Config) {
    let verdict = Firewall::new(config).review(original, candidate);
    assert!(
        verdict.accepted(),
        "firewall rejected: {:?}",
        verdict.record.fallback
    );
}

// ---------------------------------------------------------------------------

const PYTEST: &str = r#"============================= test session starts ==============================
platform linux -- Python 3.11.4, pytest-7.4.0, pluggy-1.2.0
rootdir: /repo
collected 818 items

tests/api/test_health.py ......................................          [  5%]
tests/auth/test_expiry.py ..F...................................         [ 12%]
tests/users/test_crud.py ...............F......................          [ 40%]
tests/util/test_str.py .................................              [100%]

=================================== FAILURES ===================================
______________________________ test_token_expiry _______________________________

    def test_token_expiry():
        token = make_token(ttl=-1)
>       assert validate(token).status_code == 401
E       assert 200 == 401

tests/auth/test_expiry.py:87: AssertionError
_____________________________ test_duplicate_email _____________________________

    def test_duplicate_email():
>       create_user(email="a@b.c")
E       sqlalchemy.exc.IntegrityError: duplicate key value violates unique constraint "users_email_key"

tests/users/test_crud.py:54: IntegrityError
=========================== short test summary info ============================
FAILED tests/auth/test_expiry.py::test_token_expiry - assert 200 == 401
FAILED tests/users/test_crud.py::test_duplicate_email - sqlalchemy.exc.IntegrityError
======================== 2 failed, 812 passed, 4 skipped in 71.20s =============
"#;

#[test]
fn pytest_keeps_every_failure_fact() {
    let config = cfg(Mode::Safe);
    let ctx = cmd(&["pytest", "-q"], 1);
    let candidate = compile_ok(PYTEST, Some(&ctx), &config);

    let doc = assert_ir(&candidate);
    assert_eq!(doc.kind, "test");
    assert_eq!(doc.variant.as_deref(), Some("pytest"));
    assert_eq!(doc.get("status"), Some("fail"));
    assert_eq!(doc.get("passed"), Some("812"));
    assert_eq!(doc.get("failed"), Some("2"));
    assert_eq!(doc.get("skipped"), Some("4"));

    assert_contains_all(
        &candidate,
        &[
            "tests/auth/test_expiry.py:87",
            "tests/users/test_crud.py:54",
            "AssertionError",
            "IntegrityError",
            "assert 200 == 401",
            "users_email_key",
        ],
    );
    assert_shrinks(PYTEST, &candidate);
    assert_firewall_accepts(PYTEST, candidate, &config);
}

const CARGO: &str = r#"   Compiling ttk-core v0.1.0
    Finished test [unoptimized + debuginfo] target(s) in 4.21s
     Running unittests src/lib.rs

running 818 tests
test config::tests::defaults_are_safe ... ok
test config::tests::toml_roundtrip ... ok
test auth::tests::token_expiry ... FAILED
test ir::tests::roundtrips ... ok

failures:

---- auth::tests::token_expiry stdout ----
thread 'auth::tests::token_expiry' panicked at src/auth/token.rs:118:9:
assertion `left == right` failed
  left: 200
 right: 401
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    auth::tests::token_expiry

test result: FAILED. 812 passed; 1 failed; 5 ignored; 0 measured; 0 filtered out; finished in 71.20s
"#;

#[test]
fn cargo_test_keeps_panic_location_and_values() {
    let config = cfg(Mode::Safe);
    let ctx = cmd(&["cargo", "test"], 101);
    let candidate = compile_ok(CARGO, Some(&ctx), &config);

    let doc = assert_ir(&candidate);
    assert_eq!(doc.variant.as_deref(), Some("cargo"));
    assert_eq!(doc.get("status"), Some("fail"));
    assert_eq!(doc.get("passed"), Some("812"));
    assert_eq!(doc.get("failed"), Some("1"));
    assert_eq!(doc.get("ignored"), Some("5"));

    assert_contains_all(
        &candidate,
        &[
            "auth::tests::token_expiry",
            "src/auth/token.rs:118",
            "left: 200",
            "right: 401",
        ],
    );
    assert_shrinks(CARGO, &candidate);
    assert_firewall_accepts(CARGO, candidate, &config);
}

const RUSTC: &str = r#"   Compiling ttk-core v0.1.0
error[E0433]: failed to resolve: use of undeclared crate or module `foo`
  --> src/auth/token.rs:12:5
   |
12 |     foo::bar();
   |     ^^^ use of undeclared crate or module `foo`

error[E0308]: mismatched types
  --> src/auth/token.rs:44:20
   |
44 |     let x: u32 = "nope";
   |            ---   ^^^^^^ expected `u32`, found `&str`

error: aborting due to 2 previous errors
"#;

#[test]
fn cargo_build_errors_keep_codes_and_locations() {
    let config = cfg(Mode::Safe);
    let ctx = cmd(&["cargo", "build"], 101);
    let candidate = compile_ok(RUSTC, Some(&ctx), &config);

    let doc = assert_ir(&candidate);
    assert_eq!(doc.kind, "build");
    assert_eq!(doc.get("errors"), Some("2"));
    assert_contains_all(
        &candidate,
        &[
            "E0433",
            "E0308",
            "src/auth/token.rs:12:5",
            "src/auth/token.rs:44:20",
            "mismatched types",
        ],
    );
    assert_firewall_accepts(RUSTC, candidate, &config);
}

const JEST: &str = r#"
 FAIL  src/auth/token.test.ts
  ● token expiry › rejects expired tokens

    expect(received).toBe(expected) // Object.is equality

    Expected: 401
    Received: 200

      at Object.<anonymous> (src/auth/token.test.ts:87:23)

 PASS  src/util/str.test.ts
 PASS  src/api/health.test.ts

Test Suites: 1 failed, 2 passed, 3 total
Tests:       1 failed, 812 passed, 813 total
Snapshots:   0 total
Time:        71.2 s
"#;

#[test]
fn jest_keeps_expected_and_received() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(JEST, Some(&cmd(&["npx", "jest"], 1)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("status"), Some("fail"));
    assert_eq!(doc.get("passed"), Some("812"));
    assert_eq!(doc.get("failed"), Some("1"));
    assert_contains_all(
        &candidate,
        &[
            "Expected: 401",
            "Received: 200",
            "src/auth/token.test.ts:87:23",
        ],
    );
    assert_shrinks(JEST, &candidate);
    assert_firewall_accepts(JEST, candidate, &config);
}

const GOTEST: &str = r#"=== RUN   TestHealth
--- PASS: TestHealth (0.00s)
=== RUN   TestTokenExpiry
    token_test.go:87: expected status 401, got 200
--- FAIL: TestTokenExpiry (0.01s)
=== RUN   TestCreateUser
--- PASS: TestCreateUser (0.02s)
FAIL
FAIL	example.internal/auth	0.203s
ok  	example.internal/util	0.101s
"#;

#[test]
fn go_test_keeps_failure_message() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(GOTEST, Some(&cmd(&["go", "test", "./..."], 1)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("status"), Some("fail"));
    assert_eq!(doc.get("failed"), Some("1"));
    assert_eq!(doc.get("passed"), Some("2"));
    assert_contains_all(
        &candidate,
        &[
            "TestTokenExpiry",
            "token_test.go:87",
            "expected status 401, got 200",
        ],
    );
    assert_firewall_accepts(GOTEST, candidate, &config);
}

const GIT_STATUS: &str = r#"On branch feature/token-expiry
Your branch is ahead of 'origin/main' by 3 commits.
  (use "git push" to publish your local commits)

Changes to be committed:
  (use "git restore --staged <file>..." to unstage)
	modified:   src/auth/token.rs
	new file:   src/auth/expiry.rs

Changes not staged for commit:
  (use "git add <file>..." to update what will be committed)
  (use "git restore <file>..." to discard changes in working directory)
	modified:   src/auth/middleware.rs

Untracked files:
  (use "git add <file>..." to include in what will be committed)
	notes.md

no changes added to commit (use "git add" and/or "git commit -a")
"#;

#[test]
fn git_status_keeps_every_path() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(GIT_STATUS, Some(&cmd(&["git", "status"], 0)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("branch"), Some("feature/token-expiry"));
    assert_eq!(doc.get("ahead"), Some("3"));
    assert_eq!(doc.get("files"), Some("4"));
    assert_contains_all(
        &candidate,
        &[
            "src/auth/token.rs",
            "src/auth/expiry.rs",
            "src/auth/middleware.rs",
            "notes.md",
        ],
    );
    assert_shrinks(GIT_STATUS, &candidate);
    assert_firewall_accepts(GIT_STATUS, candidate, &config);
}

const GIT_DIFF: &str = r#"diff --git a/src/auth/token.rs b/src/auth/token.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/auth/token.rs
+++ b/src/auth/token.rs
@@ -115,7 +115,7 @@ impl Token {
     pub fn is_expired(&self, now: Instant) -> bool {
         let expiry = self.issued_at + self.ttl;
         // NOTE: boundary condition matters for the 401 case
-        return expiry < now;
+        return expiry <= now;
     }

     pub fn refresh(&mut self) {
@@ -140,6 +140,9 @@ impl Token {
         self.issued_at = Instant::now();
     }

+    pub fn remaining(&self, now: Instant) -> Duration {
+        (self.issued_at + self.ttl).saturating_duration_since(now)
+    }
 }
diff --git a/README.md b/README.md
index aaa..bbb 100644
--- a/README.md
+++ b/README.md
@@ -1,3 +1,3 @@
 # project
-old line
+new line
"#;

#[test]
fn git_diff_keeps_all_changed_lines_and_hunks() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(GIT_DIFF, Some(&cmd(&["git", "diff"], 0)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("files"), Some("2"));
    assert_eq!(doc.get("added"), Some("5"));
    assert_eq!(doc.get("removed"), Some("2"));
    assert_contains_all(
        &candidate,
        &[
            "@@ -115,7 +115,7 @@",
            "@@ -140,6 +140,9 @@",
            "-        return expiry < now;",
            "+        return expiry <= now;",
            "src/auth/token.rs",
            "README.md",
        ],
    );
    assert_shrinks(GIT_DIFF, &candidate);
    assert_firewall_accepts(GIT_DIFF, candidate, &config);
}

const GIT_LOG: &str = r#"commit 9f1c2b7ad8e4f6c0b1a2d3e4f5a6b7c8d9e0f1a2
Author: Ada Lovelace <ada@example.invalid>
Date:   Mon Jul 1 10:00:00 2024 +0200

    fix: treat expiry boundary as expired

commit 1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d
Author: Grace Hopper <grace@example.invalid>
Date:   Sun Jun 30 18:22:11 2024 +0200

    refactor: extract token module
"#;

#[test]
fn git_log_keeps_hashes() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(GIT_LOG, Some(&cmd(&["git", "log"], 0)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("commits"), Some("2"));
    assert_contains_all(
        &candidate,
        &[
            "9f1c2b7a",
            "1a2b3c4d",
            "fix: treat expiry boundary as expired",
        ],
    );
    assert_shrinks(GIT_LOG, &candidate);
    assert_firewall_accepts(GIT_LOG, candidate, &config);
}

fn noisy_log() -> String {
    let mut s = String::new();
    for i in 0..500 {
        s.push_str(&format!(
            "2024-07-01T10:00:{:02}Z INFO  request handled id=req-{i} duration={}ms\n",
            i % 60,
            10 + i % 7
        ));
    }
    s.push_str(
        "2024-07-01T10:09:00Z ERROR connection to db failed after 3 retries at db/pool.rs:42\n",
    );
    s
}

#[test]
fn log_clustering_is_lossy_and_mode_gated() {
    let text = noisy_log();
    let balanced = cfg(Mode::Balanced);
    let candidate = compile_ok(&text, None, &balanced);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.kind, "log");
    assert!(candidate.lossy, "clustering must be flagged lossy");
    assert_contains_all(
        &candidate,
        &["ERROR connection to db failed after 3 retries at db/pool.rs:42"],
    );
    assert_shrinks(&text, &candidate);
    assert_firewall_accepts(&text, candidate.clone(), &balanced);

    // Safe mode must refuse the same candidate.
    let safe = cfg(Mode::Safe);
    let verdict = Firewall::new(&safe).review(&text, candidate);
    assert!(!verdict.accepted());
    assert_eq!(verdict.output, text, "rejection must restore the original");
}

#[test]
fn json_minification_is_semantically_identical() {
    let original = serde_json::to_string_pretty(&serde_json::json!({
        "users": (0..40).map(|i| serde_json::json!({
            "id": i, "email": format!("user{i}@corp.invalid"), "active": i % 2 == 0
        })).collect::<Vec<_>>(),
        "total": 40
    }))
    .expect("json");

    let config = cfg(Mode::Safe);
    let candidate = compile_ok(&original, None, &config);
    let a: serde_json::Value = serde_json::from_str(&original).expect("original");
    let b: serde_json::Value = serde_json::from_str(&candidate.output).expect("output");
    assert_eq!(a, b, "minification changed the document");
    assert_shrinks(&original, &candidate);
    assert_firewall_accepts(&original, candidate, &config);
}

#[test]
fn json_summary_only_in_lossy_modes() {
    let original = serde_json::to_string_pretty(&serde_json::json!({
        "items": (0..200).map(|i| serde_json::json!({"id": i, "name": format!("item {i}")})).collect::<Vec<_>>()
    }))
    .expect("json");

    let balanced = cfg(Mode::Balanced);
    let candidate = compile_ok(&original, None, &balanced);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.kind, "json");
    assert_eq!(doc.get("elements"), Some("200"));
    assert!(candidate.lossy);
    assert_firewall_accepts(&original, candidate, &balanced);
}

const GREP: &str = r#"src/auth/token.rs:12:use crate::auth::validate;
src/auth/token.rs:47:    validate(header)?;
src/auth/token.rs:88:    let ok = validate(&token);
src/auth/token.rs:118:    return validate_expiry(expiry, now);
src/auth/middleware.rs:42:    let token = validate(header)?;
src/auth/middleware.rs:59:    validate_scopes(&claims)?;
src/auth/middleware.rs:77:    // validate again after refresh
src/api/health.rs:7:// no validate here
src/api/health.rs:19:    validate_ping();
src/api/health.rs:31:    validate_ping();
src/util/str.rs:3:fn validate_utf8() {}
src/util/str.rs:9:    validate_utf8();
"#;

#[test]
fn grep_groups_by_file() {
    let config = cfg(Mode::Safe);
    let candidate = compile_ok(GREP, Some(&cmd(&["rg", "validate"], 0)), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.kind, "shell");
    assert_eq!(doc.get("matches"), Some("12"));
    assert_eq!(doc.get("files"), Some("4"));
    assert_contains_all(
        &candidate,
        &["src/auth/token.rs", "118:", "src/util/str.rs"],
    );
    assert_shrinks(GREP, &candidate);
    assert_firewall_accepts(GREP, candidate, &config);
}

#[test]
fn generic_shell_collapses_duplicates_and_keeps_errors() {
    let mut text = String::new();
    for _ in 0..300 {
        text.push_str("downloading package…\n");
    }
    text.push_str("error: failed to compile foo at src/lib.rs:9\n");
    let config = cfg(Mode::Balanced);
    let ctx = cmd(&["make", "build"], 2);
    let candidate = compile_ok(&text, Some(&ctx), &config);
    let doc = assert_ir(&candidate);
    assert_eq!(doc.get("exit"), Some("2"));
    assert_contains_all(&candidate, &["(x300)", "src/lib.rs:9", "make build"]);
    assert_shrinks(&text, &candidate);
    assert_firewall_accepts(&text, candidate, &config);
}

#[test]
fn unknown_content_is_left_alone() {
    let config = cfg(Mode::Maximum);
    let text = "a totally unremarkable single line";
    let input = CompileInput::new(text, &config);
    assert!(compile(&input).is_none());
}

#[test]
fn empty_output_is_left_alone() {
    let config = cfg(Mode::Maximum);
    let ctx = cmd(&["true"], 0);
    let input = CompileInput::new("", &config).command(&ctx);
    assert!(compile(&input).is_none());
}

#[test]
fn capsule_reference_is_embedded() {
    let config = cfg(Mode::Safe);
    let ctx = cmd(&["pytest"], 1);
    let input = CompileInput::new(PYTEST, &config)
        .command(&ctx)
        .capsule("cap://cap_01JEXAMPLE");
    let candidate = compile(&input).expect("candidate");
    let doc = ir::parse(&candidate.output).expect("ir");
    assert_eq!(doc.raw.as_deref(), Some("cap://cap_01JEXAMPLE"));
}
