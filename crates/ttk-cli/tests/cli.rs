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
        let dir = tempfile::tempdir().expect("tempdir");
        // `find_project_root` walks *upwards*, so without a marker of its own
        // a sandbox under the system temp directory can find a real `.ttk` or
        // `.git` further up and write into it. One empty directory here makes
        // every test hermetic regardless of what the machine looks like.
        std::fs::create_dir_all(dir.path().join(".ttk")).expect("workspace marker");
        Self { dir }
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
            // The test suite must never touch the developer's real PATH…
            .env("TTK_NO_PATH_SETUP", "1")
            // …nor their real learned-filter rules, nor their global ledger.
            .env("TTK_USER_RULES", self.dir.path().join("user-rules.json"))
            .env("TTK_GLOBAL_HOME", self.dir.path().join("global"))
            // …nor the instruction files in their real home directory.
            .env("TTK_AGENT_HOME", self.dir.path().join("agent-home"))
            // Colour is decided per test, not by whether CI has a terminal.
            .env("NO_COLOR", "1")
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
    let o = s.ttk(&["help", "--all"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    for needle in [
        "ttk run -- <command>",
        "ttk install",
        "ttk stats",
        "ttk gain",
        "ttk global",
        "ttk retrieve <capsule> --level 4",
        "WHAT IT SAVES TODAY",
        "test.pytest",
        "not a benchmark suite",
    ] {
        assert!(
            text.contains(needle),
            "`ttk help --all` is missing `{needle}`"
        );
    }
}

#[test]
fn the_agent_block_exists_once_and_updates_replace_it() {
    const BEGIN: &str = "<!-- BEGIN ThanosTokenKiller (ttk)";
    let s = Sandbox::new();
    let home = s.dir.path().join("agent-home");
    let global = home.join(".claude").join("CLAUDE.md");
    let home_level = home.join("CLAUDE.md");
    std::fs::create_dir_all(global.parent().expect("parent")).expect("mkdir");

    // The state this has to repair: an outdated global block, and a second
    // copy in ~/CLAUDE.md that Claude Code reads in every project as well.
    let stale = format!(
        "# my rules\r\n\r\n{BEGIN} — managed block, do not edit by hand -->\r\n\
         ## ThanosTokenKiller (`ttk`)\r\nold instructions\r\n<!-- END ThanosTokenKiller (ttk) -->\r\n"
    );
    std::fs::write(&global, &stale).expect("write");
    std::fs::write(
        &home_level,
        format!("# home notes\n\n{}", stale.replace("\r\n", "\n")),
    )
    .expect("write");

    let o = s.ttk(&["__installer", "refresh-agents"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = std::fs::read_to_string(&global).expect("read");
    assert_eq!(text.matches(BEGIN).count(), 1, "{text}");
    assert!(!text.contains("old instructions"), "the update replaced it");
    assert!(text.contains("ttk gain"), "with the current instructions");
    assert!(
        text.starts_with("# my rules\r\n"),
        "the user's text and CRLF stay"
    );
    let home_text = std::fs::read_to_string(&home_level).expect("read");
    assert!(
        !home_text.contains(BEGIN),
        "the duplicate is gone: {home_text}"
    );
    assert!(home_text.contains("# home notes"));

    // Running it again changes nothing.
    let o = s.ttk(&["__installer", "refresh-agents"]);
    assert!(stdout(&o).contains("already up to date"), "{}", stdout(&o));
    assert_eq!(std::fs::read_to_string(&global).expect("read"), text);

    // A project install without an explicit scope does not add a second copy
    // when the global file already has one…
    let o = s.ttk(&["install", "--target", "claude", "--yes", "--no-path"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!s.dir.path().join("CLAUDE.md").exists(), "{}", stdout(&o));
    // …but a shared repository can still ask for its own.
    let o = s.ttk(&[
        "install",
        "--target",
        "claude",
        "--scope",
        "project",
        "--yes",
        "--no-path",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(s.dir.path().join("CLAUDE.md").exists());
}

#[test]
fn a_bare_ttk_prints_the_short_overview() {
    let s = Sandbox::new();
    for args in [&[][..], &["help"][..]] {
        let o = s.ttk(args);
        assert!(o.status.success(), "{}", stderr(&o));
        let text = stdout(&o);
        assert!(text.contains("ttk run -- <command>"), "{text}");
        assert!(text.contains("ttk help --all"), "{text}");
        assert!(
            !text.contains("WHAT IT SAVES TODAY"),
            "the overview stays short"
        );
    }
}

#[test]
fn gain_totals_every_project_through_the_global_ledger() {
    let s = Sandbox::new();

    // Nothing yet: a friendly empty report, not an error.
    let empty = s.ttk(&["gain"]);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(
        stdout(&empty).contains("Nothing saved yet"),
        "{}",
        stdout(&empty)
    );

    let big = s.write(
        "big.json",
        &format!("[{}]", vec!["{\"a\": 1, \"b\": 2}"; 200].join(",")),
    );
    for _ in 0..2 {
        let o = s.ttk(&["compile", "--file", &path_arg(&big)]);
        assert!(o.status.success(), "{}", stderr(&o));
    }

    let o = s.ttk(&["gain", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["scope"], "all projects");
    assert_eq!(v["events"], 2);
    assert_eq!(v["today"]["runs"], 2, "today's runs are today's");
    assert_eq!(v["saved"], v["today"]["saved"]);

    let text = stdout(&s.ttk(&["gain"]));
    for needle in ["TOKENS SAVED", "last 14 days", "today", "global folder"] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }

    let o = s.ttk(&["gain", "--project", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["events"], 2, "the sandbox is the only project");
}

#[test]
fn the_global_folder_holds_filters_that_every_project_loads() {
    let s = Sandbox::new();
    let o = Command::new(env!("CARGO_BIN_EXE_ttk"))
        .args(["global", "--init", "--json"])
        .current_dir(s.dir.path())
        .env("TTK_HOME", s.home())
        .env("TTK_GLOBAL_HOME", s.dir.path().join("global"))
        .env_remove("TTK_USER_RULES")
        .output()
        .expect("ttk runs");
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let filters = s.dir.path().join("global").join("filters");
    assert!(filters.is_dir(), "{v}");
    assert!(s.dir.path().join("global").join("README.txt").is_file());
    assert_eq!(v["filter_files"], 0);
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

    let o = s.ttk(&["gain", "--session", "latest", "--json"]);
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
    assert!(
        stdout(&empty).contains("nothing recorded"),
        "{}",
        stdout(&empty)
    );

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
    assert!(text.contains("tokens that never reached a model"), "{text}");
    assert!(text.contains("commands run"), "{text}");
    assert!(text.contains("where the savings came from"), "{text}");
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
    assert!(text.contains("full original"), "{text}");
    assert!(text.contains("ttk retrieve"), "{text}");
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
    assert!(text.contains("replayed"), "{text}");
    assert!(text.contains("1 event(s)"), "{text}");
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

// ---------------------------------------------------------------------------
// Learned filters
// ---------------------------------------------------------------------------

/// Output that a real `npm install` produces: three lines of noise, one line
/// of substance and one error that must survive everything.
const NPM: &str = "\
npm WARN deprecated inflight@1.0.6: This module is not supported anymore
npm WARN deprecated glob@7.2.3: This module is not supported anymore
npm WARN deprecated rimraf@2.7.1: This module is not supported anymore
added 412 packages in 9s
npm ERR! code ELIFECYCLE
";

const LESSON: &str = "\
<filter-trash>
npm WARN deprecated inflight@1.0.6: This module is not supported anymore
npm WARN deprecated glob@7.2.3: This module is not supported anymore
</filter-trash>
added 412 packages in 9s
";

/// The whole promise of the feature, end to end through the binary: mark the
/// junk once, and it is gone from every later run of the same command.
#[test]
fn teaching_once_removes_the_noise_from_every_later_run() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);

    let learned = s.ttk(&[
        "learn",
        "--file",
        &path_arg(&lesson),
        "--scope",
        "npm install",
        "--json",
    ]);
    assert!(learned.status.success(), "{}", stderr(&learned));
    let v: serde_json::Value = serde_json::from_str(&stdout(&learned)).expect("json");
    assert_eq!(v["scope"], "npm install");
    assert_eq!(
        v["created"].as_array().expect("created").len(),
        1,
        "two lines of the same shape are one rule: {v}"
    );

    // Now filter output that has *different* versions in it.
    let output = s.write("run.txt", NPM);
    let filtered = s.ttk(&[
        "filter",
        "--file",
        &path_arg(&output),
        "--scope",
        "npm install",
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&filtered)).expect("json");
    let content = v["content"].as_str().expect("content");
    assert!(!content.contains("deprecated"), "{content}");
    assert!(content.contains("added 412 packages"));
    assert!(
        content.contains("npm ERR!"),
        "an error line is never filtered: {content}"
    );
    assert_eq!(v["removed_lines"], 3, "all three shapes match the one rule");
}

#[test]
fn a_lesson_with_no_tags_explains_the_tags() {
    let s = Sandbox::new();
    let plain = s.write("plain.txt", "just some output\n");
    let o = s.ttk(&["learn", "--file", &path_arg(&plain)]);
    assert!(!o.status.success());
    let text = stderr(&o);
    assert!(text.contains("<filter-trash>"), "{text}");
    assert!(text.contains("counter-example"), "{text}");
}

#[test]
fn errors_are_refused_with_a_reason_and_can_be_forced() {
    let s = Sandbox::new();
    let lesson = s.write(
        "err.txt",
        "<filter-trash>\nFAILED tests/a.py::test_x - assert 200 == 401\n</filter-trash>\n",
    );

    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert!(v["created"].as_array().expect("created").is_empty());
    let reason = v["rejected"][0]["reason"].as_str().expect("reason");
    assert!(reason.contains("looks like an error"), "{reason}");

    let forced = s.ttk(&[
        "learn",
        "--file",
        &path_arg(&lesson),
        "--global",
        "--force",
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&forced)).expect("json");
    assert_eq!(v["created"].as_array().expect("created").len(), 1);
}

#[test]
fn an_over_general_rule_is_refused_because_of_what_you_kept() {
    let s = Sandbox::new();
    // Two shapes that differ in one word, and a third line of that same shape
    // left unmarked. Widening would eat it, so widening must not happen.
    let lesson = s.write(
        "wide.txt",
        "Downloading package critical from the registry cache\n\
         <filter-trash>\n\
         Downloading package alpha from the registry cache\n\
         Downloading package beta from the registry cache\n\
         </filter-trash>\n",
    );
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["generalised"], 0);
    assert_eq!(
        v["created"].as_array().expect("created").len(),
        2,
        "both narrow shapes are kept instead of one wide one"
    );

    let probe = s.write(
        "probe.txt",
        "Downloading package critical from the registry cache\n",
    );
    let filtered = s.ttk(&["filter", "--file", &path_arg(&probe), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&filtered)).expect("json");
    assert_eq!(v["removed_lines"], 0, "the kept line survives");
}

#[test]
fn a_dry_run_writes_nothing() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    let o = s.ttk(&[
        "learn",
        "--file",
        &path_arg(&lesson),
        "--global",
        "--dry-run",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("dry run"));
    assert!(
        !s.home().join("learned-filters.json").exists(),
        "a dry run must not create the rule file"
    );
}

#[test]
fn rules_can_be_listed_disabled_enabled_and_forgotten() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let id = v["created"][0]["id"].as_str().expect("id").to_string();

    assert!(stdout(&s.ttk(&["rules"])).contains(&id));
    assert!(stdout(&s.ttk(&["rules", "show", &id])).contains("how to read the pattern"));

    assert!(s.ttk(&["rules", "disable", &id]).status.success());
    let listed = stdout(&s.ttk(&["rules", "--json"]));
    assert!(
        !listed.contains(&id),
        "a disabled rule is hidden by default"
    );
    assert!(stdout(&s.ttk(&["rules", "list", "--all", "--json"])).contains(&id));

    assert!(s.ttk(&["rules", "enable", &id]).status.success());
    assert!(stdout(&s.ttk(&["rules", "--json"])).contains(&id));

    assert!(s.ttk(&["rules", "forget", &id]).status.success());
    assert!(!stdout(&s.ttk(&["rules", "list", "--all", "--json"])).contains(&id));
}

#[test]
fn rules_survive_an_export_import_round_trip() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    let exported = stdout(&s.ttk(&["rules", "export"]));
    assert!(exported.contains("schema_version"));
    let file = s.write("export.json", &exported);

    let other = Sandbox::new();
    let o = other.ttk(&["rules", "import", "--file", &path_arg(&file), "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["added"], 1);
    // Importing the same file again adds nothing: the id is the identity.
    let again = other.ttk(&["rules", "import", "--file", &path_arg(&file), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&again)).expect("json");
    assert_eq!(v["added"], 0);
}

#[test]
fn a_filter_keep_tag_retires_the_rule_that_was_wrong() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    let correction = s.write(
        "keep.txt",
        "<filter-keep>npm WARN deprecated other@2.0.0: This module is not supported anymore</filter-keep>\n\
         <filter-trash>\nsome unrelated noise line to keep the lesson valid here\n</filter-trash>\n",
    );
    let o = s.ttk(&[
        "learn",
        "--file",
        &path_arg(&correction),
        "--global",
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(
        v["retired"].as_array().expect("retired").len(),
        1,
        "the rule that would have removed the kept line is disabled: {v}"
    );
}

#[test]
fn compiling_through_the_pipeline_applies_and_credits_the_rules() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    let output = s.write("run.txt", NPM);
    let o = s.ttk(&["compile", "--file", &path_arg(&output), "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert!(
        !v["content"]
            .as_str()
            .expect("content")
            .contains("deprecated")
    );
    assert_eq!(v["filter"]["removed_lines"], 3);

    // The rule's own books were updated by that run.
    let listed = stdout(&s.ttk(&["rules", "--json"]));
    let rules: serde_json::Value = serde_json::from_str(&listed).expect("json");
    assert_eq!(rules[0]["hits"], 1);
    assert_eq!(rules[0]["lines_removed"], 3);
    assert!(rules[0]["tokens_saved"].as_u64().expect("tokens") > 0);
}

#[test]
fn observe_mode_filters_nothing() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    let output = s.write("run.txt", NPM);
    let o = s.ttk(&[
        "--mode",
        "observe",
        "compile",
        "--file",
        &path_arg(&output),
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert!(
        v["content"]
            .as_str()
            .expect("content")
            .contains("deprecated"),
        "observe mode never changes a byte"
    );
}

#[test]
fn the_rule_file_is_readable_json_meant_for_review() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    let text = std::fs::read_to_string(s.home().join("learned-filters.json")).expect("rule file");
    assert!(text.contains("\"pattern\""), "{text}");
    assert!(
        text.contains("npm WARN deprecated"),
        "a reviewer must be able to read the rule: {text}"
    );
    assert!(text.lines().count() > 5, "pretty printed, not one line");
}

#[test]
fn colour_is_off_in_a_pipe_and_forced_on_demand() {
    let s = Sandbox::new();
    // stdout is captured, never a terminal, so `auto` must stay plain.
    assert!(!stdout(&s.ttk(&["stats"])).contains('\u{1b}'));
    let forced = Command::new(env!("CARGO_BIN_EXE_ttk"))
        .args(["--color", "always", "stats"])
        .current_dir(s.dir.path())
        .env("TTK_HOME", s.home())
        .env("TTK_USER_RULES", s.dir.path().join("user-rules.json"))
        .env_remove("NO_COLOR")
        .output()
        .expect("ttk runs");
    assert!(
        stdout(&forced).contains('\u{1b}'),
        "--color always must style"
    );
}

/// The scope a `--last` lesson derives has to match what the *next* run
/// reports, or the rule is stored and never fires. Only an end to end round
/// trip catches a mismatch between the two, so this test drives the real loop:
/// run, teach from that run, run again.
#[test]
fn a_lesson_taught_from_the_last_run_fires_on_the_next_one() {
    let s = Sandbox::new();
    let noisy = s.write("out.txt", NPM);
    let show: Vec<String> = if cfg!(windows) {
        vec![
            "run".into(),
            "--".into(),
            "cmd".into(),
            "/C".into(),
            format!("type {}", path_arg(&noisy)),
        ]
    } else {
        vec![
            "run".into(),
            "--".into(),
            "sh".into(),
            "-c".into(),
            format!("cat {}", path_arg(&noisy)),
        ]
    };
    let argv: Vec<&str> = show.iter().map(String::as_str).collect();
    assert!(s.ttk(&argv).status.success());

    // Teach from that run: no --scope, so the scope comes from the command.
    let lesson = s.write("lesson.txt", LESSON);
    let taught = s.ttk(&["learn", "--last", "--file", &path_arg(&lesson), "--json"]);
    assert!(taught.status.success(), "{}", stderr(&taught));
    let v: serde_json::Value = serde_json::from_str(&stdout(&taught)).expect("json");
    assert_eq!(v["created"].as_array().expect("created").len(), 1, "{v}");

    // The same command again: the rule must actually fire this time.
    let again = s.ttk(&argv);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(
        !stdout(&again).contains("deprecated"),
        "the rule did not fire — stdout: {}\nstderr: {}",
        stdout(&again),
        stderr(&again)
    );
    assert!(stdout(&again).contains("added 412 packages"));
    assert!(
        stderr(&again).contains("learned filter"),
        "{}",
        stderr(&again)
    );
}

// ---------------------------------------------------------------------------
// Block rules
// ---------------------------------------------------------------------------

/// A banner: decoration lines carry one token each, so no line rule can
/// describe them and the run has to be learned as a whole.
const BANNER: &str = "\
+========================================+
|                                        |
|      ACME TOOLCHAIN, version 4.2       |
|                                        |
+========================================+
";

#[test]
fn a_banner_becomes_one_block_rule_that_removes_the_whole_run() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", &format!("<filter-trash>\n{BANNER}</filter-trash>\ncompiling module alpha\nbuild finished in 4.1s\n"));

    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["blocks"], 1, "{v}");
    let block = v["created"]
        .as_array()
        .expect("created")
        .iter()
        .find(|r| r["block"] == true)
        .expect("a block rule");
    assert_eq!(block["height"], 5);
    assert!(
        block["pattern"].as_str().expect("pattern").contains('\n'),
        "a block pattern is several lines"
    );

    // The whole banner goes, in one run, and the real output stays.
    let log = s.write(
        "build.log",
        &format!("{BANNER}compiling module alpha\nbuild finished in 4.1s\n"),
    );
    let filtered = s.ttk(&["filter", "--file", &path_arg(&log), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&filtered)).expect("json");
    assert_eq!(v["removed_lines"], 5);
    let content = v["content"].as_str().expect("content");
    assert!(!content.contains("ACME TOOLCHAIN"), "{content}");
    assert!(content.contains("build finished"));
    let hit = v["by_rule"]
        .as_array()
        .expect("by_rule")
        .iter()
        .find(|h| h["id"] == block["id"])
        .expect("the block fired");
    assert_eq!(hit["runs"], 1, "one run, five lines");
    assert_eq!(hit["lines"], 5);
}

#[test]
fn a_block_needs_the_whole_run_to_be_there() {
    let s = Sandbox::new();
    let lesson = s.write(
        "lesson.txt",
        &format!("<filter-trash>\n{BANNER}</filter-trash>\nreal output\n"),
    );
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);

    // The middle of the banner is gone, so the run is not there any more.
    let mut lines: Vec<&str> = BANNER.lines().collect();
    lines.remove(2);
    let partial = s.write(
        "partial.log",
        &format!("{}\nreal output\n", lines.join("\n")),
    );
    let o = s.ttk(&["filter", "--file", &path_arg(&partial), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(
        v["removed_lines"], 0,
        "four fifths of a banner is not the banner: {v}"
    );
}

#[test]
fn a_block_is_refused_when_the_run_carries_an_error() {
    let s = Sandbox::new();
    let lesson = s.write(
        "lesson.txt",
        "<filter-trash>\n\
         ==========================================\n\
         FAILED tests/a.py::test_x - assert 200 == 401\n\
         ==========================================\n\
         </filter-trash>\n\
         real output\n",
    );
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["blocks"], 0);
    assert!(v["created"].as_array().expect("created").is_empty());
    let reasons: Vec<&str> = v["rejected"]
        .as_array()
        .expect("rejected")
        .iter()
        .map(|r| r["reason"].as_str().unwrap_or(""))
        .collect();
    assert!(
        reasons.iter().any(|r| r.contains("looks like an error")),
        "{reasons:?}"
    );
}

#[test]
fn repeated_noise_does_not_turn_into_a_block() {
    let s = Sandbox::new();
    let lesson = s.write("lesson.txt", LESSON);
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(
        v["blocks"], 0,
        "one line rule already describes the run: {v}"
    );
    assert_eq!(v["created"][0]["block"], false);
    assert_eq!(v["created"][0]["kind"], "drop");
}

#[test]
fn a_block_rule_survives_export_and_import() {
    let s = Sandbox::new();
    let lesson = s.write(
        "lesson.txt",
        &format!("<filter-trash>\n{BANNER}</filter-trash>\nreal output\n"),
    );
    s.ttk(&["learn", "--file", &path_arg(&lesson), "--global"]);
    let exported = stdout(&s.ttk(&["rules", "export"]));
    // The block is a list of lines on disk, and it stays readable.
    assert!(exported.contains("ACME TOOLCHAIN"), "{exported}");
    let file = s.write("export.json", &exported);

    let other = Sandbox::new();
    assert!(
        other
            .ttk(&["rules", "import", "--file", &path_arg(&file)])
            .status
            .success()
    );
    let log = other.write("build.log", &format!("{BANNER}real output\n"));
    let o = other.ttk(&["filter", "--file", &path_arg(&log), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["removed_lines"], 5, "the imported block still fires");
}

#[test]
fn block_learning_can_be_switched_off_in_the_config() {
    let s = Sandbox::new();
    std::fs::write(
        s.dir.path().join(".ttk").join("config.toml"),
        "[learning]\nlearn_blocks = false\n",
    )
    .expect("write config");

    let lesson = s.write(
        "lesson.txt",
        &format!("<filter-trash>\n{BANNER}</filter-trash>\nreal output\n"),
    );
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["blocks"], 0);
    let reasons: Vec<&str> = v["rejected"]
        .as_array()
        .expect("rejected")
        .iter()
        .map(|r| r["reason"].as_str().unwrap_or(""))
        .collect();
    assert!(
        reasons.iter().any(|r| r.contains("learn_blocks is off")),
        "the reason has to name the setting: {reasons:?}"
    );
}

// ---------------------------------------------------------------------------
// The global usage ledger
// ---------------------------------------------------------------------------

#[test]
fn every_project_lands_in_one_global_ledger() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    for _ in 0..2 {
        assert!(
            s.ttk(&["compile", "--file", &path_arg(&fixture)])
                .status
                .success()
        );
    }

    let o = s.ttk(&["stats", "--global", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["events"], 2);
    assert!(v["saved"].as_u64().expect("saved") > 0);
    assert_eq!(v["projects"].as_array().expect("projects").len(), 1);
    assert!(
        s.dir.path().join("global").join("usage.jsonl").is_file(),
        "the ledger lives outside the workspace"
    );

    // The human rendering leads with the number and names the projects.
    let text = stdout(&s.ttk(&["stats", "--global"]));
    assert!(text.contains("everywhere"), "{text}");
    assert!(text.contains("by project"), "{text}");
}

#[test]
fn the_global_ledger_survives_compaction_without_losing_totals() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    for _ in 0..3 {
        s.ttk(&["compile", "--file", &path_arg(&fixture)]);
    }
    let before: serde_json::Value =
        serde_json::from_str(&stdout(&s.ttk(&["stats", "--global", "--json"]))).expect("json");

    let o = s.ttk(&["usage", "compact", "--retain-days", "0", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let compacted: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert!(compacted["records_after"].as_u64().expect("after") < 3);

    let after: serde_json::Value =
        serde_json::from_str(&stdout(&s.ttk(&["stats", "--global", "--json"]))).expect("json");
    assert_eq!(after["saved"], before["saved"], "totals must not move");
    assert_eq!(after["events"], before["events"]);
}

#[test]
fn a_project_can_be_forgotten_from_the_ledger() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    s.ttk(&["compile", "--file", &path_arg(&fixture)]);
    assert!(s.ttk(&["usage", "forget", "--all"]).status.success());

    let v: serde_json::Value =
        serde_json::from_str(&stdout(&s.ttk(&["stats", "--global", "--json"]))).expect("json");
    assert_eq!(v["events"], 0);
}

#[test]
fn the_ledger_can_be_switched_off_entirely() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = Command::new(env!("CARGO_BIN_EXE_ttk"))
        .args(["compile", "--file", &path_arg(&fixture)])
        .current_dir(s.dir.path())
        .env("TTK_HOME", s.home())
        .env("TTK_GLOBAL_HOME", s.dir.path().join("global"))
        .env("TTK_USAGE", "0")
        .output()
        .expect("ttk runs");
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        !s.dir.path().join("global").join("usage.jsonl").exists(),
        "TTK_USAGE=0 must write nothing"
    );
}

// ---------------------------------------------------------------------------
// Saying "the same as last time"
// ---------------------------------------------------------------------------

fn echo(script: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "run".into(),
            "--".into(),
            "cmd".into(),
            "/C".into(),
            script.into(),
        ]
    } else {
        vec![
            "run".into(),
            "--".into(),
            "sh".into(),
            "-c".into(),
            script.into(),
        ]
    }
}

#[test]
fn an_identical_second_run_collapses_to_a_pointer() {
    let s = Sandbox::new();
    let passing = s.write(
        "pass.txt",
        "running 4 tests\ntest alpha ... ok\ntest beta ... ok\n\
         test gamma ... ok\ntest delta ... ok\n\
         test result: ok. 4 passed; 0 failed; finished in 0.31s\n",
    );
    let script = format!(
        "{} {}",
        if cfg!(windows) { "type" } else { "cat" },
        path_arg(&passing)
    );
    let argv: Vec<String> = echo(&script);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();

    let first = s.ttk(&args);
    assert!(first.status.success(), "{}", stderr(&first));

    let mut json_args = args.clone();
    json_args.insert(0, "--json");
    let second = s.ttk(&json_args);
    assert!(second.status.success(), "{}", stderr(&second));
    let v: serde_json::Value = serde_json::from_str(&stdout(&second)).expect("json");
    assert_eq!(v["winner"], "repeat.suppress", "{v}");
    let content = v["content"].as_str().expect("content");
    assert!(content.starts_with("[repeat]"), "{content}");
    assert!(
        v["tokens_after"].as_u64().expect("after") < v["tokens_before"].as_u64().expect("before"),
        "{v}"
    );
}

/// The whole safety argument in one test: repeating is not forgetting.
#[test]
fn an_identical_failing_run_still_shows_the_failure() {
    let s = Sandbox::new();
    let failing = s.write(
        "fail.txt",
        "running 2 tests\ntest alpha ... ok\ntest beta ... FAILED\n\
         thread 'beta' panicked at src/lib.rs:42: assertion failed: 1 == 2\n\
         test result: FAILED. 1 passed; 1 failed\n",
    );
    let script = format!(
        "{} {}",
        if cfg!(windows) { "type" } else { "cat" },
        path_arg(&failing)
    );
    let argv = echo(&script);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();

    s.ttk(&args);
    let mut json_args = args.clone();
    json_args.insert(0, "--json");
    let second = s.ttk(&json_args);
    let v: serde_json::Value = serde_json::from_str(&stdout(&second)).expect("json");
    let content = v["content"].as_str().expect("content");
    assert!(
        content.contains("assertion failed: 1 == 2"),
        "an identical failing run must still show the failure: {content}"
    );
    assert!(content.contains("src/lib.rs:42"), "{content}");
}

#[test]
fn a_first_run_is_never_a_repeat() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_ne!(v["winner"], "repeat.suppress");
}

#[test]
fn piped_input_is_never_matched_against_an_earlier_run() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    for _ in 0..2 {
        let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
        let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
        assert_ne!(
            v["winner"], "repeat.suppress",
            "without a command we cannot claim two inputs are the same thing"
        );
    }
}

#[test]
fn a_capsule_reference_is_short_and_still_resolves() {
    let s = Sandbox::new();
    let fixture = s.write("pytest.txt", PYTEST);
    let o = s.ttk(&["compile", "--file", &path_arg(&fixture), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let reference = v["capsule"].as_str().expect("capsule");
    assert!(
        reference.len() <= "cap://".len() + 12,
        "the reference is printed on every output: {reference}"
    );

    // …and everything that takes a capsule accepts it.
    let handle = reference.trim_start_matches("cap://");
    let raw = s.ttk(&["retrieve", handle, "--level", "4"]);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert!(stdout(&raw).contains("818 items"), "{}", stdout(&raw));
    assert!(stdout(&s.ttk(&["capsule", "list"])).contains(handle));
}

// ---------------------------------------------------------------------------
// Reading files, suggesting lessons, folding, whitelisting, onboarding
// ---------------------------------------------------------------------------

const SOURCE: &str = "\
//! A module.

use std::fmt;

pub struct Config {
    pub mode: String,
}

impl Config {
    pub fn load(path: &str) -> Config {
        let mut value = String::new();
        value.push_str(path);
        Config { mode: value }
    }

    fn helper(&self) -> bool {
        true
    }
}

pub fn build() -> Config {
    Config::load(\"x\")
}
";

#[test]
fn an_outline_shows_the_declarations_and_points_at_the_rest() {
    let s = Sandbox::new();
    let file = s.write("config.rs", SOURCE);
    let o = s.ttk(&["read", "--outline", &path_arg(&file), "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let content = v["content"].as_str().expect("content");

    assert!(content.starts_with("[outline]"), "{content}");
    assert!(content.contains("pub struct Config"), "{content}");
    assert!(
        content.contains("pub fn load(path: &str) -> Config"),
        "{content}"
    );
    assert!(content.contains("lines_not_shown="), "{content}");
    assert!(
        !content.contains("value.push_str"),
        "a body is not a signature"
    );
    assert!(
        v["tokens_after"].as_u64().expect("after") < v["tokens_before"].as_u64().expect("before")
    );

    // The whole file is one retrieve away, by the handle the outline printed.
    let handle = v["capsule"].as_str().expect("capsule");
    let raw = s.ttk(&["retrieve", handle, "--level", "4"]);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert!(stdout(&raw).contains("value.push_str"));
}

#[test]
fn reading_the_same_unchanged_file_twice_costs_a_pointer() {
    let s = Sandbox::new();
    let file = s.write("config.rs", SOURCE);
    assert!(s.ttk(&["read", &path_arg(&file)]).status.success());

    let o = s.ttk(&["read", &path_arg(&file), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["winner"], "repeat.suppress", "{v}");
}

#[test]
fn a_line_range_is_a_plain_numbered_slice() {
    let s = Sandbox::new();
    let file = s.write("config.rs", SOURCE);
    let o = s.ttk(&["read", &path_arg(&file), "--lines", "5:7"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(text.contains("5: pub struct Config"), "{text}");
    assert!(!text.contains("1: //! A module."), "{text}");
}

#[test]
fn suggest_drafts_a_lesson_it_never_applies() {
    let s = Sandbox::new();
    let noisy = s.write("out.txt", NPM);
    let script = format!(
        "{} {}",
        if cfg!(windows) { "type" } else { "cat" },
        path_arg(&noisy)
    );
    let argv = echo(&script);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    for _ in 0..3 {
        s.ttk(&args);
    }

    let o = s.ttk(&["suggest", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    let items = v["suggestions"].as_array().expect("suggestions");
    assert!(!items.is_empty(), "{v}");
    assert!(
        items
            .iter()
            .all(|i| !i["example"].as_str().unwrap_or("").contains("npm ERR!")),
        "an error line is never suggested for deletion: {v}"
    );
    // Nothing was learned.
    let rules: serde_json::Value =
        serde_json::from_str(&stdout(&s.ttk(&["rules", "list", "--all", "--json"]))).expect("json");
    assert_eq!(rules.as_array().expect("rules").len(), 0);

    // …and the draft is valid input for `ttk learn`.
    let draft = stdout(&s.ttk(&["suggest", "--lesson"]));
    assert!(draft.contains("<filter-trash>"), "{draft}");
    assert!(draft.contains("Nothing here has been learned"), "{draft}");
}

#[test]
fn a_fold_rule_keeps_a_summary_instead_of_nothing() {
    let s = Sandbox::new();
    let lesson = s.write(
        "lesson.txt",
        "<filter-fold as=\"npm: deprecation warnings\">\n\
         npm WARN deprecated inflight@1.0.6: This module is not supported anymore\n\
         npm WARN deprecated glob@7.2.3: This module is not supported anymore\n\
         </filter-fold>\n\
         added 412 packages in 9s\n",
    );
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["folds"], 1, "{v}");
    assert_eq!(v["created"][0]["kind"], "fold");

    let output = s.write("run.txt", NPM);
    let filtered = s.ttk(&["filter", "--file", &path_arg(&output), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&filtered)).expect("json");
    let content = v["content"].as_str().expect("content");
    assert!(
        content.contains("[folded] npm: deprecation warnings (3 line(s))"),
        "{content}"
    );
    assert!(
        content.contains("npm ERR!"),
        "errors still survive: {content}"
    );
    assert_eq!(v["folded_runs"], 1);
}

#[test]
fn a_whitelist_keeps_only_what_it_names_and_always_the_errors() {
    let s = Sandbox::new();
    let lesson = s.write(
        "lesson.txt",
        "<filter-only>\nadded 412 packages in 9s\n</filter-only>\n",
    );
    let o = s.ttk(&["learn", "--file", &path_arg(&lesson), "--global", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("json");
    assert_eq!(v["keeps"], 1, "{v}");

    let output = s.write("run.txt", NPM);
    let filtered = s.ttk(&["filter", "--file", &path_arg(&output), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&filtered)).expect("json");
    let content = v["content"].as_str().expect("content");
    assert!(content.contains("added 412 packages"), "{content}");
    assert!(
        content.contains("npm ERR!"),
        "the whitelist never outranks the error guard: {content}"
    );
    assert!(!content.contains("deprecated"), "{content}");
    assert!(v["whitelisted_away"].as_u64().expect("away") >= 3, "{v}");
}

#[test]
fn setup_walks_through_everything_and_is_idempotent() {
    let s = Sandbox::new();
    let first = s.ttk(&["setup", "--yes", "--no-path", "--no-demo"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let text = stdout(&first);
    assert!(text.contains("[1/5]"), "{text}");
    assert!(text.contains("[5/5]"), "{text}");
    assert!(s.dir.path().join("CLAUDE.md").is_file());
    assert!(s.dir.path().join("AGENTS.md").is_file());
    assert!(s.dir.path().join(".ttk").join("config.toml").is_file());

    // Running it again changes nothing and says so.
    let again = s.ttk(&["setup", "--yes", "--no-path", "--no-demo"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(
        stdout(&again).contains("already up to date") || stdout(&again).contains("already exists"),
        "{}",
        stdout(&again)
    );
}

#[test]
fn the_compact_agent_block_is_a_fraction_of_the_full_one() {
    let full = Sandbox::new();
    full.ttk(&[
        "install", "--target", "claude", "--scope", "project", "--yes",
    ]);
    let long = std::fs::read_to_string(full.dir.path().join("CLAUDE.md")).expect("CLAUDE.md");

    let small = Sandbox::new();
    small.ttk(&[
        "install",
        "--target",
        "claude",
        "--scope",
        "project",
        "--yes",
        "--compact",
    ]);
    let short = std::fs::read_to_string(small.dir.path().join("CLAUDE.md")).expect("CLAUDE.md");

    assert!(
        short.len() * 3 < long.len(),
        "{} vs {}",
        short.len(),
        long.len()
    );
    // …and still teaches the loop.
    assert!(short.contains("ttk run --"));
    assert!(short.contains("<filter-trash>"));
    assert!(short.contains("ttk retrieve"));
}
