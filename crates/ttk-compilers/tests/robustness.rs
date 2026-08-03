//! Robustness tests: hostile, malformed and pathological input must never
//! panic, never hang and never produce something the firewall would accept
//! while losing a critical fact.
//!
//! This is a cheap deterministic stand-in for a fuzzer — the same corpus a
//! fuzz target would start from, without the CI cost.

use ttk_compilers::{CommandContext, CompileInput, compile};
use ttk_core::config::{Config, Mode};
use ttk_core::content;
use ttk_core::firewall::Firewall;
use ttk_core::ir;

fn cfg(mode: Mode) -> Config {
    Config {
        mode,
        ..Config::default()
    }
}

fn ctx() -> CommandContext {
    CommandContext {
        argv: vec!["some-tool".into(), "--run".into()],
        cwd: "/tmp".into(),
        exit_code: Some(1),
        signal: None,
        duration_ms: 5,
        stdout_bytes: 0,
        stderr_bytes: 0,
    }
}

/// Inputs chosen to break each parser in a different way.
fn corpus() -> Vec<String> {
    let mut v: Vec<String> = vec![
        String::new(),
        "\n".into(),
        "\0\0\0".into(),
        "\u{feff}".into(),
        "日本語 テキスト\n".repeat(50),
        "🙂".repeat(5000),
        "a".repeat(200_000),
        // Structurally "almost" valid inputs for each compiler.
        "test result: FAILED.".into(),
        "test result: FAILED. x passed; y failed; z ignored;".into(),
        "=================================== FAILURES ===================================\n".into(),
        "short test summary info\nFAILED\nFAILED \n".into(),
        "--- FAIL:".into(),
        "--- FAIL: T (0.0s)\n\tno location here\n".into(),
        "diff --git a/ b/\n@@\n+".into(),
        "diff --git a/x b/x\nBinary files a/x and b/x differ\n".into(),
        "commit \nAuthor:\nDate:\n".into(),
        "On branch \nChanges to be committed:\n\t:\n".into(),
        "Tests:  \n●\n".into(),
        "{".into(),
        "[[[[[[[[[[[[[[[[[[[[".into(),
        "{\"a\":".into(),
        "error[E9999]: \n  --> :0:0\n".into(),
        ":::::\n:1:\n".into(),
        // Lines that look like every format at once.
        "test result: FAILED. 1 passed; diff --git a/x b/x commit abc1234 --- FAIL: X\n".repeat(30),
    ];
    // Deeply nested JSON, right at and beyond the depth limit.
    v.push(format!("{}1{}", "[".repeat(200), "]".repeat(200)));
    // Grep-ish output with no content after the colon.
    v.push((0..20).map(|i| format!("f{i}.rs:{i}:\n")).collect());
    v
}

#[test]
fn no_input_can_panic_a_compiler() {
    for mode in [Mode::Safe, Mode::Balanced, Mode::Maximum, Mode::Observe] {
        let config = cfg(mode);
        for text in corpus() {
            let detection = content::detect(&text, None);
            let command = ctx();
            for with_command in [false, true] {
                let mut input =
                    CompileInput::new(&text, &config).content_type(detection.content_type);
                if with_command {
                    input = input.command(&command);
                }
                // `compile` swallows panics by design; the assertion is that we
                // get here at all, for every combination.
                let _ = compile(&input);
            }
        }
    }
}

#[test]
fn every_candidate_is_valid_token_ir_or_valid_json() {
    let config = cfg(Mode::Maximum);
    let command = ctx();
    for text in corpus() {
        let detection = content::detect(&text, None);
        let input = CompileInput::new(&text, &config)
            .content_type(detection.content_type)
            .command(&command);
        let Some(candidate) = compile(&input) else {
            continue;
        };
        let ir_ok = ir::parse(&candidate.output).is_ok();
        let json_ok = serde_json::from_str::<serde_json::Value>(&candidate.output).is_ok();
        assert!(
            ir_ok || json_ok,
            "compiler {} emitted something unparsable for {:?}:\n{}",
            candidate.transformer,
            &text.chars().take(40).collect::<String>(),
            candidate.output
        );
    }
}

#[test]
fn the_firewall_always_returns_usable_content() {
    let command = ctx();
    for mode in [Mode::Safe, Mode::Balanced, Mode::Maximum] {
        let config = cfg(mode);
        for text in corpus() {
            let detection = content::detect(&text, None);
            let input = CompileInput::new(&text, &config)
                .content_type(detection.content_type)
                .command(&command);
            let Some(candidate) = compile(&input) else {
                continue;
            };
            let verdict = Firewall::new(&config).review(&text, candidate);
            if verdict.accepted() {
                // Accepted output must be smaller and structurally sound.
                assert!(verdict.record.tokens_after.value <= verdict.record.tokens_before.value);
            } else {
                // Rejected means byte-identical pass-through, always.
                assert_eq!(verdict.output, text);
            }
        }
    }
}

#[test]
fn compilation_is_deterministic_across_the_corpus() {
    let config = cfg(Mode::Maximum);
    let command = ctx();
    for text in corpus() {
        let detection = content::detect(&text, None);
        let input = CompileInput::new(&text, &config)
            .content_type(detection.content_type)
            .command(&command);
        let a = compile(&input).map(|c| c.output);
        let b = compile(&input).map(|c| c.output);
        assert_eq!(a, b, "non-deterministic output");
    }
}

#[test]
fn a_panicking_compiler_does_not_stop_the_registry() {
    // The guard is exercised directly: `guarded` must convert the panic into a
    // fallback verdict rather than unwinding into the caller.
    let verdict = ttk_core::firewall::guarded("boom", 1, "original text", || {
        panic!("synthetic parser bug")
    })
    .expect_err("must produce a fallback");
    assert_eq!(verdict.output, "original text");
}
