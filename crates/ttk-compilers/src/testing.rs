//! Test framework compilers.
//!
//! Test output is the single most wasteful thing an agent reads: thousands of
//! lines of "PASSED" for the two lines that matter. These compilers keep
//!
//! * the framework and the overall status,
//! * every counter the framework actually reported (never an invented one),
//! * for each failing test: its name, its location, the error type and the
//!   expected/actual values, verbatim.
//!
//! Everything else goes into the capsule.

use std::sync::LazyLock;

use regex::Regex;
use ttk_core::firewall::{Candidate, Reparse};
use ttk_core::invariants::{ExtractPolicy, Invariant, InvariantKind};
use ttk_core::ir::{IrDoc, IrSection};

use crate::{CompileInput, Compiler, clip, lines_capped};

/// Upper bound on lines we parse, so a runaway test log cannot stall the CLI.
const MAX_LINES: usize = 200_000;
/// Failures rendered in full. The rest is counted and left in the capsule.
const MAX_FAILURES: usize = 25;
/// Detail lines kept per failure.
const MAX_DETAIL: usize = 12;

#[derive(Debug, Default, Clone)]
struct Failure {
    name: String,
    location: Option<String>,
    error_type: Option<String>,
    detail: Vec<String>,
}

#[derive(Debug, Default)]
struct TestRun {
    framework: &'static str,
    status: Option<&'static str>,
    counts: Vec<(&'static str, u64)>,
    duration: Option<String>,
    failures: Vec<Failure>,
    /// Failures found beyond [`MAX_FAILURES`].
    omitted_failures: usize,
}

impl TestRun {
    fn is_empty(&self) -> bool {
        self.counts.is_empty() && self.failures.is_empty() && self.status.is_none()
    }

    fn status_str(&self) -> &'static str {
        self.status.unwrap_or(if self.failures.is_empty() {
            "unknown"
        } else {
            "fail"
        })
    }

    fn to_ir(&self, input: &CompileInput<'_>) -> IrDoc {
        let mut doc = IrDoc::new("test")
            .variant(self.framework)
            .head("status", self.status_str());
        for (k, v) in &self.counts {
            doc = doc.field_num(*k, v);
        }
        if let Some(d) = &self.duration {
            doc = doc.field("duration", d.clone());
        }
        if let Some(cmd) = input.command {
            doc = doc.field("cmd", cmd.command_line());
            if let Some(code) = cmd.exit_code {
                doc = doc.field_num("exit", code);
            }
        }
        for (i, f) in self.failures.iter().enumerate() {
            let mut sec = IrSection::new(format!("fail#{}", i + 1)).attr("test", f.name.clone());
            if let Some(loc) = &f.location {
                sec = sec.attr("at", loc.clone());
            }
            if let Some(t) = &f.error_type {
                sec = sec.attr("error", t.clone());
            }
            sec = sec.lines(f.detail.iter().map(|l| clip(l, 400)));
            doc = doc.section(sec);
        }
        if self.omitted_failures > 0 {
            doc = doc.field_num("failures_not_shown", self.omitted_failures);
        }
        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }
        doc
    }

    /// Invariants the compiler guarantees: every failing test's location,
    /// error type and detail lines.
    fn invariants(&self) -> Vec<Invariant> {
        let mut out = Vec::new();
        for f in &self.failures {
            out.push(Invariant::new(InvariantKind::Custom, f.name.clone()));
            if let Some(loc) = &f.location {
                out.push(Invariant::new(InvariantKind::LineRef, loc.clone()));
            }
            if let Some(t) = &f.error_type {
                out.push(Invariant::new(InvariantKind::ErrorType, t.clone()));
            }
            for line in &f.detail {
                out.extend(ttk_core::invariants::extract(
                    line,
                    &ExtractPolicy::critical(),
                ));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn candidate(
    id: &'static str,
    version: u32,
    run: &TestRun,
    input: &CompileInput<'_>,
) -> Option<Candidate> {
    if run.is_empty() {
        return None;
    }
    let doc = run.to_ir(input);
    Some(
        Candidate::new(id, version, doc.render())
            .invariants(run.invariants())
            .reparse(Reparse::TokenIr)
            .note(format!(
                "{} failing test(s) kept verbatim, full output in the capsule",
                run.failures.len()
            )),
    )
}

// ---------------------------------------------------------------------------
// pytest
// ---------------------------------------------------------------------------

static PYTEST_COUNT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (passed|failed|error|errors|skipped|xfailed|xpassed|deselected|warnings?)")
        .expect("static regex")
});
static PYTEST_DURATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"in ([\d.]+)s").expect("static regex"));
static PYTEST_SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^_{2,} (.+?) _{2,}$").expect("static regex"));
static PYTEST_LOCATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^\s:]+:\d+): (\w+)").expect("static regex"));

pub struct PytestCompiler;

impl Compiler for PytestCompiler {
    fn id(&self) -> &'static str {
        "test.pytest"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        let c = input.content;
        c.contains("test session starts")
            || c.contains("short test summary info")
            || (c.contains("=== FAILURES ===") && c.contains(".py"))
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut run = TestRun {
            framework: "pytest",
            ..Default::default()
        };

        // Summary counters live on the last `=====` banner line.
        for line in lines_capped(input.content, MAX_LINES) {
            if !line.starts_with('=') || !PYTEST_COUNT_RE.is_match(line) {
                continue;
            }
            run.counts.clear();
            for caps in PYTEST_COUNT_RE.captures_iter(line) {
                let n: u64 = caps[1].parse().unwrap_or(0);
                let key = match &caps[2] {
                    "passed" => "passed",
                    "failed" => "failed",
                    "error" | "errors" => "errors",
                    "skipped" => "skipped",
                    "xfailed" => "xfailed",
                    "xpassed" => "xpassed",
                    "deselected" => "deselected",
                    _ => "warnings",
                };
                run.counts.push((key, n));
            }
            if let Some(d) = PYTEST_DURATION_RE.captures(line) {
                let secs = &d[1];
                run.duration = Some(format!("{secs}s"));
            }
        }
        run.status = Some(
            if run
                .counts
                .iter()
                .any(|(k, v)| matches!(*k, "failed" | "errors") && *v > 0)
            {
                "fail"
            } else if run.counts.is_empty() {
                return None;
            } else {
                "pass"
            },
        );

        // Failure blocks.
        let mut current: Option<Failure> = None;
        let mut seen = 0usize;
        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(caps) = PYTEST_SECTION_RE.captures(line.trim_end()) {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                current = Some(Failure {
                    name: caps[1].trim().to_string(),
                    ..Default::default()
                });
                continue;
            }
            let Some(f) = current.as_mut() else { continue };

            if let Some(rest) = line.strip_prefix("E ") {
                if f.detail.len() < MAX_DETAIL {
                    f.detail.push(rest.trim().to_string());
                }
            } else if let Some(caps) = PYTEST_LOCATION_RE.captures(line) {
                f.location = Some(caps[1].to_string());
                f.error_type = Some(caps[2].to_string());
            } else if line.starts_with("===")
                && let Some(f) = current.take()
            {
                push_failure(&mut run, f, &mut seen);
            }
        }
        if let Some(f) = current.take() {
            push_failure(&mut run, f, &mut seen);
        }

        // The short summary carries the node ids; use them to enrich names.
        for line in lines_capped(input.content, MAX_LINES) {
            let Some(rest) = line
                .strip_prefix("FAILED ")
                .or_else(|| line.strip_prefix("ERROR "))
            else {
                continue;
            };
            let nodeid = rest.split_whitespace().next().unwrap_or(rest);
            let short = nodeid.rsplit("::").next().unwrap_or(nodeid);
            if let Some(f) = run.failures.iter_mut().find(|f| f.name == short) {
                f.name = nodeid.to_string();
            } else if run.failures.len() < MAX_FAILURES {
                run.failures.push(Failure {
                    name: nodeid.to_string(),
                    detail: rest
                        .split_once(" - ")
                        .map(|(_, m)| vec![m.trim().to_string()])
                        .unwrap_or_default(),
                    ..Default::default()
                });
            }
        }

        candidate(self.id(), self.version(), &run, input)
    }
}

fn push_failure(run: &mut TestRun, f: Failure, seen: &mut usize) {
    *seen += 1;
    if run.failures.len() < MAX_FAILURES {
        run.failures.push(f);
    } else {
        run.omitted_failures += 1;
    }
}

// ---------------------------------------------------------------------------
// cargo test / libtest
// ---------------------------------------------------------------------------

static CARGO_RESULT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out; finished in ([\d.]+)s",
    )
    .expect("static regex")
});
static CARGO_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---- (.+) stdout ----$").expect("static regex"));
static CARGO_PANIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at ([^\s:]+(?::\d+){1,2}):?").expect("static regex"));
static RUSTC_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^error(?:\[(E\d{4})\])?: (.+)$").expect("static regex"));
static RUSTC_LOCATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*--> (.+:\d+:\d+)$").expect("static regex"));

pub struct CargoTestCompiler;

impl Compiler for CargoTestCompiler {
    fn id(&self) -> &'static str {
        "test.cargo"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        let c = input.content;
        c.contains("test result:")
            || (c.contains("running ") && c.contains(" test"))
            || RUSTC_ERROR_RE.is_match(c)
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        if !input.content.contains("test result:") && RUSTC_ERROR_RE.is_match(input.content) {
            return self.compile_build_errors(input);
        }

        let mut run = TestRun {
            framework: "cargo",
            ..Default::default()
        };
        let (mut passed, mut failed, mut ignored, mut measured, mut filtered) = (0, 0, 0, 0, 0);
        let mut duration = 0.0f64;
        let mut any = false;
        let mut all_ok = true;

        for line in lines_capped(input.content, MAX_LINES) {
            let Some(caps) = CARGO_RESULT_RE.captures(line) else {
                continue;
            };
            any = true;
            all_ok &= &caps[1] == "ok";
            passed += caps[2].parse::<u64>().unwrap_or(0);
            failed += caps[3].parse::<u64>().unwrap_or(0);
            ignored += caps[4].parse::<u64>().unwrap_or(0);
            measured += caps[5].parse::<u64>().unwrap_or(0);
            filtered += caps[6].parse::<u64>().unwrap_or(0);
            duration += caps[7].parse::<f64>().unwrap_or(0.0);
        }
        if !any {
            return None;
        }
        run.status = Some(if all_ok { "pass" } else { "fail" });
        run.counts = vec![("passed", passed), ("failed", failed), ("ignored", ignored)];
        if measured > 0 {
            run.counts.push(("measured", measured));
        }
        if filtered > 0 {
            run.counts.push(("filtered_out", filtered));
        }
        run.duration = Some(format!("{duration:.2}s"));

        let mut current: Option<Failure> = None;
        let mut seen = 0usize;
        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(caps) = CARGO_BLOCK_RE.captures(line.trim_end()) {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                current = Some(Failure {
                    name: caps[1].to_string(),
                    ..Default::default()
                });
                continue;
            }
            let Some(f) = current.as_mut() else { continue };
            if line.starts_with("failures:") || line.starts_with("test result:") {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                continue;
            }
            if let Some(caps) = CARGO_PANIC_RE.captures(line) {
                f.location = Some(caps[1].to_string());
                f.error_type.get_or_insert_with(|| "panic".to_string());
            }
            let t = line.trim();
            if !t.is_empty() && f.detail.len() < MAX_DETAIL {
                f.detail.push(t.to_string());
            }
        }
        if let Some(f) = current.take() {
            push_failure(&mut run, f, &mut seen);
        }

        candidate(self.id(), self.version(), &run, input)
    }
}

impl CargoTestCompiler {
    /// `cargo build` / `cargo check` diagnostics.
    fn compile_build_errors(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        struct BuildError {
            code: String,
            message: String,
            at: Option<String>,
            /// `expected`/`found`, `help:` and `note:` lines, verbatim.
            detail: Vec<String>,
        }

        let mut errors: Vec<BuildError> = Vec::new();
        for line in lines_capped(input.content, MAX_LINES) {
            let trimmed = line.trim_end();
            if trimmed.starts_with("error: aborting") {
                continue;
            }
            if let Some(caps) = RUSTC_ERROR_RE.captures(trimmed) {
                errors.push(BuildError {
                    code: caps
                        .get(1)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                    message: caps[2].to_string(),
                    at: None,
                    detail: Vec::new(),
                });
                continue;
            }
            let Some(current) = errors.last_mut() else {
                continue;
            };
            if let Some(caps) = RUSTC_LOCATION_RE.captures(trimmed) {
                current.at.get_or_insert_with(|| caps[1].to_string());
                continue;
            }
            // Keep the annotation lines that carry the actual type mismatch.
            let t = trimmed.trim_start_matches(['|', ' ', '^', '-']).trim();
            if t.is_empty() || current.detail.len() >= 4 {
                continue;
            }
            if t.contains("expected ")
                || t.contains("found ")
                || t.starts_with("help:")
                || t.starts_with("note:")
            {
                current.detail.push(t.to_string());
            }
        }

        // `error: ...` alone is far too generic — any Makefile prints that.
        // A rustc diagnostic always carries a `--> file:line:col` location.
        let count = errors.len();
        if count == 0 || !errors.iter().any(|e| e.at.is_some()) {
            return None;
        }

        let mut doc = IrDoc::new("build").variant("cargo").head("status", "error");
        let mut invariants = Vec::new();
        for (i, e) in errors.iter().take(MAX_FAILURES).enumerate() {
            let mut sec = IrSection::new(format!("error#{}", i + 1));
            if let Some(at) = &e.at {
                sec = sec.attr("at", at.clone());
                invariants.push(Invariant::new(InvariantKind::LineRef, at.clone()));
            }
            if !e.code.is_empty() {
                sec = sec.attr("code", e.code.clone());
                invariants.push(Invariant::new(InvariantKind::ErrorType, e.code.clone()));
            }
            invariants.push(Invariant::new(InvariantKind::Custom, e.message.clone()));
            sec = sec.line(clip(&e.message, 400));
            for d in &e.detail {
                invariants.push(Invariant::new(InvariantKind::Custom, d.clone()));
                sec = sec.line(clip(d, 400));
            }
            doc = doc.section(sec);
        }
        doc = doc.field_num("errors", count);
        if count > MAX_FAILURES {
            doc = doc.field_num("errors_not_shown", count - MAX_FAILURES);
        }
        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }
        invariants.sort();
        invariants.dedup();
        Some(
            Candidate::new("build.cargo", self.version(), doc.render())
                .invariants(invariants)
                .reparse(Reparse::TokenIr),
        )
    }
}

// ---------------------------------------------------------------------------
// jest / vitest
// ---------------------------------------------------------------------------

static JEST_COUNT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (passed|failed|skipped|todo|pending)").expect("static regex")
});
static JEST_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"at .*?\(?([\w./\\-]+\.[jt]sx?:\d+:\d+)\)?").expect("static regex")
});

pub struct JestCompiler;

impl Compiler for JestCompiler {
    fn id(&self) -> &'static str {
        "test.jest"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        let c = input.content;
        (c.contains("Tests:") || c.contains("Test Files") || c.contains("Test Suites:"))
            && (c.contains("✓")
                || c.contains("✕")
                || c.contains("●")
                || c.contains("PASS")
                || c.contains("FAIL"))
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut run = TestRun {
            framework: if input.content.contains("Test Files") {
                "vitest"
            } else {
                "jest"
            },
            ..Default::default()
        };

        for line in lines_capped(input.content, MAX_LINES) {
            let t = line.trim_start();
            if !(t.starts_with("Tests:") || t.starts_with("Tests ")) {
                continue;
            }
            run.counts.clear();
            for caps in JEST_COUNT_RE.captures_iter(t) {
                let n: u64 = caps[1].parse().unwrap_or(0);
                let key = match &caps[2] {
                    "passed" => "passed",
                    "failed" => "failed",
                    "skipped" => "skipped",
                    "todo" => "todo",
                    _ => "pending",
                };
                run.counts.push((key, n));
            }
        }
        if run.counts.is_empty() {
            return None;
        }
        run.status = Some(
            if run.counts.iter().any(|(k, v)| *k == "failed" && *v > 0) {
                "fail"
            } else {
                "pass"
            },
        );

        let mut current: Option<Failure> = None;
        let mut seen = 0usize;
        let mut current_file: Option<String> = None;
        for line in lines_capped(input.content, MAX_LINES) {
            let t = line.trim_end();
            if let Some(rest) = t.trim_start().strip_prefix("FAIL ") {
                current_file = Some(rest.trim().to_string());
            }
            if let Some(rest) = t.trim_start().strip_prefix("● ") {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                if rest.starts_with("Console") || rest.is_empty() {
                    continue;
                }
                current = Some(Failure {
                    name: rest.trim().to_string(),
                    location: current_file.clone(),
                    ..Default::default()
                });
                continue;
            }
            let Some(f) = current.as_mut() else { continue };
            if let Some(caps) = JEST_AT_RE.captures(t) {
                f.location = Some(caps[1].to_string());
            }
            let tt = t.trim();
            if tt.is_empty() || f.detail.len() >= MAX_DETAIL {
                continue;
            }
            if tt.starts_with("Expected")
                || tt.starts_with("Received")
                || tt.starts_with("expect(")
                || tt.starts_with("AssertionError")
                || tt.starts_with("Error:")
                || tt.starts_with("- Expected")
                || tt.starts_with("+ Received")
            {
                if f.error_type.is_none() && tt.starts_with("expect(") {
                    f.error_type = Some("assertion".to_string());
                }
                f.detail.push(tt.to_string());
            }
        }
        if let Some(f) = current.take() {
            push_failure(&mut run, f, &mut seen);
        }

        candidate(self.id(), self.version(), &run, input)
    }
}

// ---------------------------------------------------------------------------
// go test
// ---------------------------------------------------------------------------

/// `token_test.go:87: expected 401, got 200` → location `token_test.go:87`.
fn add_go_detail(f: &mut Failure, line: &str) {
    if f.detail.len() >= MAX_DETAIL {
        return;
    }
    if f.location.is_none()
        && let Some(idx) = line.find(".go:")
    {
        let start = line[..idx]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let after = idx + ".go:".len();
        let digits = line[after..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|o| after + o)
            .unwrap_or(line.len());
        if digits > after {
            f.location = Some(line[start..digits].to_string());
        }
    }
    f.detail.push(line.to_string());
}

static GO_FAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*--- (FAIL|PASS|SKIP): (\S+)").expect("static regex"));

pub struct GoTestCompiler;

impl Compiler for GoTestCompiler {
    fn id(&self) -> &'static str {
        "test.go"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        let c = input.content;
        c.contains("--- FAIL:") || c.contains("--- PASS:") || c.contains("=== RUN")
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let mut run = TestRun {
            framework: "go",
            ..Default::default()
        };
        let (mut passed, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut current: Option<Failure> = None;
        let mut seen = 0usize;
        // `go test` prints a test's `t.Errorf` output *before* the
        // `--- FAIL:` verdict line, so indented lines are buffered until the
        // verdict tells us which test they belonged to.
        let mut pending: Vec<String> = Vec::new();

        for line in lines_capped(input.content, MAX_LINES) {
            if let Some(caps) = GO_FAIL_RE.captures(line) {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                match &caps[1] {
                    "FAIL" => {
                        failed += 1;
                        let mut f = Failure {
                            name: caps[2].to_string(),
                            ..Default::default()
                        };
                        for line in pending.drain(..) {
                            add_go_detail(&mut f, &line);
                        }
                        current = Some(f);
                    }
                    "SKIP" => {
                        skipped += 1;
                        pending.clear();
                    }
                    _ => {
                        passed += 1;
                        pending.clear();
                    }
                }
                continue;
            }

            let indented = line.starts_with(' ') || line.starts_with('\t');
            if !indented {
                if let Some(f) = current.take() {
                    push_failure(&mut run, f, &mut seen);
                }
                pending.clear();
                continue;
            }
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            match current.as_mut() {
                Some(f) => add_go_detail(f, t),
                None => {
                    if pending.len() < MAX_DETAIL {
                        pending.push(t.to_string());
                    }
                }
            }
        }
        if let Some(f) = current.take() {
            push_failure(&mut run, f, &mut seen);
        }

        if failed == 0 && passed == 0 && skipped == 0 {
            return None;
        }
        run.status = Some(if failed > 0 { "fail" } else { "pass" });
        // Only report counters we actually observed: without `-v`, go prints
        // no `--- PASS` lines at all.
        if passed > 0 {
            run.counts.push(("passed", passed));
        }
        run.counts.push(("failed", failed));
        if skipped > 0 {
            run.counts.push(("skipped", skipped));
        }

        candidate(self.id(), self.version(), &run, input)
    }
}
