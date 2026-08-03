//! End to end tests against the real `ttk` binary.
//!
//! Each test gets its own isolated workspace via `TTK_HOME`, so the suite can
//! never touch a developer's real capsule store.

use std::path::Path;
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("home")
    }

    fn ttk(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ttk"))
            .args(args)
            .current_dir(self.dir.path())
            .env("TTK_HOME", self.home())
            .env("TTK_SESSION", "se_TESTSESSION000000000000000")
            // The test suite must never touch the developer's real PATH.
            .env("TTK_NO_PATH_SETUP", "1")
            .env_remove("TTK_MODE")
            .output()
            .expect("ttk runs")
    }

    fn write(&self, name: &str, content: &str) -> std::path::PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, content).expect("write fixture");
        p
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn path_arg(p: &Path) -> String {
    p.display().to_string()
}

const PYTEST: &str = "============================= test session starts ==============================\n\
platform linux -- Python 3.11.4, pytest-7.4.0\n\
collected 818 items\n\
\n\
tests/api/test_health.py ...............................\n\
tests/auth/test_expiry.py ..F............................\n\
\n\
=================================== FAILURES ===================================\n\
______________________________ test_token_expiry _______________________________\n\
\n\
    def test_token_expiry():\n\
>       assert validate(tok).status_code == 401\n\
E       assert 200 == 401\n\
\n\
tests/auth/test_expiry.py:87: AssertionError\n\
=========================== short test summary info ============================\n\
FAILED tests/auth/test_expiry.py::test_token_expiry - assert 200 == 401\n\
======================== 1 failed, 812 passed, 4 skipped in 71.20s =============\n";

#[test]
fn doctor_reports_a_healthy_installation() {
    let s = Sandbox::new();
    let o = s.ttk(&["doctor"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("all checks passed"), "{out}");
    assert!(out.contains("mode             safe"), "{out}");
}

#[test]
fn help_prints_the_cheat_sheet_and_the_savings_table() {
    let s = Sandbox::new();
    let o = s.ttk(&["help"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    for needle in [
        "ttk run -- <command>",
        "ttk install",
        "ttk stats",
        "ttk retrieve <capsule> --level 4",
        "WHAT IT SAVES TODAY",
        "test.pytest",
        "not a benchmark suite",
    ] {
        assert!(text.contains(needle), "`ttk help` is missing `{needle}`");
    }
}

#[test]
fn install_adds_a_managed_block_without_touching_existing_rules() {
    let s = Sandbox::new();
    s.write("CLAUDE.md", "# My rules\n\nAlways run the tests.\n");

    let o = s.ttk(&["install", "--target", "all", "--scope", "project", "--yes"]);
    assert!(o.status.success(), "{}", stderr(&o));

    let claude = std::fs::read_to_string(s.dir.path().join("CLAUDE.md")).expect("CLAUDE.md");
    assert!(claude.starts_with("# My rules\n\nAlways run the tests.\n"));
    assert!(claude.contains("ttk run -- cargo test"));
    assert!(claude.contains("Bash tool"), "Claude Code wording expected");
    assert!(claude.contains("ttk retrieve"), "retrieval must be taught");

    let agents = std::fs::read_to_string(s.dir.path().join("AGENTS.md")).expect("AGENTS.md");
    assert!(agents.contains("shell command"), "Codex wording expected");

    // Running it again must be a no-op.
    let again = s.ttk(&["install", "--target", "all", "--yes"]);
    assert!(again.status.success());
    assert!(
        stdout(&again).contains("already set up"),
        "{}",
        stdout(&again)
    );
    let claude_again = std::fs::read_to_string(s.dir.path().join("CLAUDE.md")).expect("read");
    assert_eq!(claude, claude_again);
}

#[test]
fn install_dry_run_writes_nothing() {
    let s = Sandbox::new();
    let o = s.ttk(&["install", "--target", "claude", "--dry-run"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("dry run: nothing was written"));
    assert!(!s.dir.path().join("CLAUDE.md").exists());
}

#[test]
fn install_reports_targets_as_json() {
    let s = Sandbox::new();
    let o = s.ttk(&["--json", "install", "--target", "codex", "--yes"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let rows = v["files"].as_array().expect("files array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["agent"], "Codex");
    assert_eq!(rows[0]["action"], "created");
    assert_eq!(rows[0]["global"], false);
    assert_eq!(v["dry_run"], false);
    // The sandbox disables the PATH step, and that is reported, not hidden.
    assert_eq!(v["path_setup"]["outcome"], "skipped");
    assert!(
        v["path_setup"]["detail"]
            .as_str()
            .expect("detail")
            .contains("TTK_NO_PATH_SETUP")
    );
}

#[test]
fn install_can_be_told_to_leave_path_alone() {
    let s = Sandbox::new();
    let o = s.ttk(&[
        "--json",
        "install",
        "--target",
        "claude",
        "--yes",
        "--no-path",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert!(
        v["path_setup"].is_null(),
        "--no-path must not attempt any PATH change: {}",
        v["path_setup"]
    );
    // The agent file is still written.
    assert!(s.dir.path().join("CLAUDE.md").is_file());
}

#[test]
fn install_announces_the_path_change_before_asking() {
    let s = Sandbox::new();
    // --dry-run still prints the plan; the sandbox skips the actual write.
    let o = s.ttk(&["install", "--target", "claude", "--dry-run"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("user PATH"), "{text}");
    assert!(
        text.contains("never with `setx`"),
        "the reason must be visible to the user: {text}"
    );
}

#[test]
fn install_rejects_an_unknown_agent_and_never_writes() {
    let s = Sandbox::new();
    let o = s.ttk(&["install", "--target", "emacs", "--yes"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("unknown install target"));
    assert!(!s.dir.path().join("CLAUDE.md").exists());
}

#[test]
fn install_without_a_terminal_explains_the_flags() {
    let s = Sandbox::new();
    // No --target and no TTY: must fail with guidance rather than hang.
    let o = s.ttk(&["install"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--target"), "{}", stderr(&o));
}

#[test]
fn init_writes_a_config_and_gitignore() {
    let s = Sandbox::new();
    let o = s.ttk(&["init"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let config = s.dir.path().join(".ttk").join("config.toml");
    assert!(config.is_file(), "{}", stdout(&o));
    let text = std::fs::read_to_string(&config).expect("read");
    assert!(text.contains("mode = \"safe\""), "{text}");
    assert!(s.dir.path().join(".ttk").join(".gitignore").is_file());
}

#[test]
fn compile_produces_token_ir_and_a_retrievable_capsule() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);

    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json output");

    let content = v["content"].as_str().expect("content");
    assert!(content.starts_with("[test:pytest]"), "{content}");
    assert!(content.contains("tests/auth/test_expiry.py:87"));
    assert!(content.contains("assert 200 == 401"));

    let before = v["tokens_before"].as_u64().expect("before");
    let after = v["tokens_after"].as_u64().expect("after");
    assert!(
        after * 2 < before,
        "expected a real reduction: {before} -> {after}"
    );
    assert_eq!(v["tokens_method"], "estimated");

    // The original must come back byte for byte.
    let capsule = v["capsule"].as_str().expect("capsule");
    let o = s.ttk(&["retrieve", capsule, "--level", "4"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), PYTEST);
}

#[test]
fn observe_mode_never_changes_a_byte() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&[
        "--mode",
        "observe",
        "compile",
        "--file",
        &path_arg(&fixture),
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), PYTEST);
    // …while still reporting what it could have saved.
    assert!(stderr(&o).contains("tokens"), "{}", stderr(&o));
}

#[test]
fn run_forwards_the_child_exit_code() {
    let s = Sandbox::new();
    let argv: Vec<&str> = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "exit 7"]
    } else {
        vec!["run", "--", "sh", "-c", "exit 7"]
    };
    let o = s.ttk(&argv);
    assert_eq!(o.status.code(), Some(7), "{}", stderr(&o));
}

#[test]
fn run_records_a_session_that_gain_can_report() {
    let s = Sandbox::new();
    let script = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo hello"]
    } else {
        vec!["run", "--", "sh", "-c", "echo hello"]
    };
    assert!(s.ttk(&script).status.success());

    let o = s.ttk(&["gain", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["events"], 1);
    assert_eq!(v["capsules_written"], 1);
    assert_eq!(v["method"], "estimated");
}

#[test]
fn stats_report_lifetime_totals() {
    let s = Sandbox::new();

    // Nothing recorded yet.
    let empty = s.ttk(&["stats"]);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(stdout(&empty).contains("nothing recorded yet"));

    let fixture = s.write("pytest.txt", PYTEST);
    for _ in 0..2 {
        assert!(
            s.ttk(&["compile", "--file", &path_arg(&fixture)])
                .status
                .success()
        );
    }
    let run = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo hi"]
    } else {
        vec!["run", "--", "sh", "-c", "echo hi"]
    };
    assert!(s.ttk(&run).status.success());

    let o = s.ttk(&["stats", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["events"], 3);
    assert_eq!(
        v["commands_run"], 1,
        "only `ttk run` events carry a command"
    );
    assert_eq!(v["sessions"], 1);
    // One capsule per event; the two identical pytest runs share a blob but
    // stay separate records, because they are separate events.
    assert_eq!(v["capsules"], 3);
    assert_eq!(v["method"], "estimated");
    assert!(
        v["tokens_before"].as_u64().expect("before") > v["tokens_after"].as_u64().expect("after")
    );
    assert!(
        v["by_transformer"]
            .as_array()
            .expect("transformers")
            .iter()
            .any(|t| t["transformer"] == "test.pytest")
    );

    let text = stdout(&s.ttk(&["stats"]));
    assert!(text.contains("tokens saved"), "{text}");
    assert!(text.contains("commands run"), "{text}");
    assert!(text.contains("top commands"), "{text}");
    assert!(text.contains('~'), "estimates must stay marked: {text}");
}

#[test]
fn secrets_are_replaced_and_raw_access_is_gated() {
    let s = Sandbox::new();
    let fixture = s.write(
        "log.txt",
        "starting up\nAuthorization: Bearer eyJhbGciOi.eyJzdWIiOiJ4.QWxhZGRpbg\ndone\n",
    );
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let capsule = v["capsule"].as_str().expect("capsule");

    let denied = s.ttk(&["raw", capsule]);
    assert_eq!(denied.status.code(), Some(9), "{}", stderr(&denied));
    assert!(stderr(&denied).contains("--allow-secrets"));

    let allowed = s.ttk(&["raw", capsule, "--allow-secrets"]);
    assert!(allowed.status.success());
    assert!(stdout(&allowed).contains("eyJhbGciOi.eyJzdWIiOiJ4.QWxhZGRpbg"));
}

#[test]
fn unknown_capsule_fails_with_a_stable_exit_code() {
    let s = Sandbox::new();
    let o = s.ttk(&["retrieve", "cap_doesnotexist"]);
    assert_eq!(o.status.code(), Some(5), "{}", stderr(&o));
    assert!(stderr(&o).contains("not found"));
}

#[test]
fn an_invalid_mode_is_rejected_before_anything_runs() {
    let s = Sandbox::new();
    let o = s.ttk(&["--mode", "turbo", "doctor"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("unknown mode"));
}

#[test]
fn explain_describes_the_transformation() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let event = v["event"].as_str().expect("event id");

    let o = s.ttk(&["explain", event]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("test.pytest"), "{text}");
    assert!(text.contains("invariant"), "{text}");
    assert!(text.contains("full original: ttk retrieve"), "{text}");
}

#[test]
fn search_inside_a_capsule() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let capsule = v["capsule"].as_str().expect("capsule");

    let o = s.ttk(&["search", capsule, "collected"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("collected 818 items"));

    let miss = s.ttk(&["search", capsule, "definitely-not-in-there"]);
    assert_eq!(miss.status.code(), Some(1));
}

#[test]
fn replay_compares_against_the_recorded_run() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    assert!(
        s.ttk(&["compile", "--file", &path_arg(&fixture)])
            .status
            .success()
    );

    let o = s.ttk(&["replay"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("replayed 1 event"), "{text}");
    assert!(text.contains("identical to the recorded run"), "{text}");
}

#[test]
fn capsule_maintenance_commands_work() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let capsule = v["capsule"]
        .as_str()
        .expect("capsule")
        .replace("cap://", "");

    assert!(stdout(&s.ttk(&["capsule", "list"])).contains(&capsule));
    assert!(stdout(&s.ttk(&["capsule", "stats"])).contains("capsules"));
    assert!(s.ttk(&["capsule", "gc"]).status.success());

    let del = s.ttk(&["capsule", "delete", &capsule]);
    assert!(del.status.success(), "{}", stderr(&del));
    assert_eq!(
        s.ttk(&["capsule", "delete", &capsule]).status.code(),
        Some(1)
    );
}

#[test]
fn config_shows_layers_and_json() {
    let s = Sandbox::new();
    let o = s.ttk(&["config", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["config"]["mode"], "safe");
    assert!(
        v["layers"]
            .as_array()
            .expect("layers")
            .iter()
            .any(|l| l == "default")
    );
}

#[test]
fn a_broken_project_config_is_reported_with_the_file_name() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.dir.path().join(".ttk")).expect("mkdir");
    std::fs::write(
        s.dir.path().join(".ttk").join("config.toml"),
        "mdoe = \"safe\"\n",
    )
    .expect("write");
    let o = s.ttk(&["doctor"]);
    let text = stdout(&o) + &stderr(&o);
    assert!(text.contains("mdoe"), "{text}");
}
