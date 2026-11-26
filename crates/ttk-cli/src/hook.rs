//! `ttk hook` — the agent integration that does not depend on the agent
//! remembering anything.
//!
//! Markdown instructions are advice; a hook is enforcement. Claude Code calls
//! `ttk hook` with a JSON document on stdin for each registered event:
//!
//! | event | what ttk does |
//! |---|---|
//! | `PostToolUse` (Bash, PowerShell) | compiles the output and hands the agent the compiled view (`updatedToolOutput`); records it |
//! | `SessionStart` (startup, resume, clear, compact) | injects the core rules, so they survive compaction |
//! | `SubagentStart` | injects the core rules into every subagent |
//! | `Stop`, `SubagentStop`, `SessionEnd` | imports the provider's measured usage from the transcript |
//!
//! The one rule above all others: **a hook never breaks the agent.** Every
//! failure ends in exit code 0 and no output, which Claude Code reads as "no
//! opinion" — the agent gets the original tool output, unchanged. Failures are
//! written to `<global>/hook.log` instead.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use ttk_compilers::CommandContext;
use ttk_core::config::Config;
use ttk_core::ids::SessionId;
use ttk_core::{Error, Result, tokens};
use ttk_store::{RecordKind, Workspace};

use crate::attribution::{self, Attribution, Delivery};
use crate::pipeline::{self, PipelineInput};

/// Tools whose output is a shell command's.
const SHELL_TOOLS: [&str; 2] = ["Bash", "PowerShell"];

/// Entry point: read the event, act on it, never fail.
pub fn run() -> i32 {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return 0;
    }
    match handle(&raw) {
        Ok(Some(out)) => {
            println!("{out}");
        }
        Ok(None) => {}
        Err(e) => log_error(&format!("{e}")),
    }
    0
}

/// Handle one event; `Some` is the JSON to print.
pub fn handle(raw: &str) -> Result<Option<Value>> {
    let input: Value =
        serde_json::from_str(raw).map_err(|e| Error::other(format!("hook input: {e}")))?;
    let event = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let config = ttk_core::config::load(&cwd)
        .map(|l| l.config)
        .unwrap_or_default();
    let attr = attribution_of(&input);

    match event {
        "PostToolUse" => post_tool_use(&input, &cwd, &config, &attr),
        "SessionStart" => Ok(inject(&input, &cwd, &config, &attr, "SessionStart")),
        "SubagentStart" => Ok(inject(&input, &cwd, &config, &attr, "SubagentStart")),
        "Stop" | "SessionEnd" => {
            import_usage(&config, input.get("transcript_path"));
            Ok(None)
        }
        "SubagentStop" => {
            import_usage(&config, input.get("agent_transcript_path"));
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn attribution_of(input: &Value) -> Attribution {
    Attribution {
        session: str_field(input, "session_id"),
        agent: Some("claude-code".to_string()),
        agent_id: str_field(input, "agent_id"),
        agent_type: str_field(input, "agent_type"),
        model: str_field(input, "model"),
        via: "hook",
    }
}

// ---------------------------------------------------------------------------
// PostToolUse
// ---------------------------------------------------------------------------

fn post_tool_use(
    input: &Value,
    cwd: &Path,
    config: &Config,
    attr: &Attribution,
) -> Result<Option<Value>> {
    if !config.hooks.compile_output {
        return Ok(None);
    }
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if !SHELL_TOOLS.contains(&tool) {
        return Ok(None);
    }
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    let command = tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if command.trim().is_empty() || already_ttk(&command) {
        // `ttk run` / `ttk retrieve` record themselves; compiling their
        // output again would count it twice.
        return Ok(None);
    }
    if tool_input
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(response) = input.get("tool_response").filter(|v| v.is_object()) else {
        return Ok(None);
    };
    if response.get("isImage").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let stdout = response.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = response.get("stderr").and_then(Value::as_str).unwrap_or("");
    let content = if stderr.trim().is_empty() {
        stdout.to_string()
    } else if stdout.trim().is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };
    let id = str_field(input, "tool_use_id");
    let context = command_context(&command, cwd);
    let before = tokens::estimate(&content).value;

    // Small outputs: recorded, never touched. A compiled view and a capsule
    // pointer cost tokens of their own, and below this line they cost more
    // than they could save.
    if before < config.hooks.min_tokens {
        attribution::record(
            attr,
            Delivery {
                id,
                program: context.program(),
                tokens_before: before,
                tokens_after: before,
                overhead: 0,
                cause: "output",
                filtered_lines: 0,
                kind: RecordKind::Passthrough,
                cwd,
            },
        );
        return Ok(None);
    }

    let ws = Workspace::open(cwd, config)?;
    let rules = if config.learning.enabled {
        ttk_learn::Layered::load(ws.root()).ok()
    } else {
        None
    };
    let session = attr
        .session
        .as_deref()
        .map(SessionId::from_string)
        .unwrap_or_default();
    let mut pin = PipelineInput::new(&content, session).command(&context);
    if let Some(r) = &rules {
        pin = pin.rules(&r.merged);
    }
    let out = pipeline::process(&ws, config, pin)?;
    if let Some(r) = &rules {
        let _ = r;
        let _ = crate::commands::record_filter_hits(&ws, config, &out);
    }

    let after_full = out.event.tokens_after().value;
    let worth_it = (after_full as f64) <= before as f64 * (1.0 - config.hooks.min_saving);
    let delivered = if worth_it { after_full } else { before };

    let mut hint = None;
    if config.hooks.hints
        && before >= config.hooks.hint_min_tokens
        && (before - delivered) * 4 < before
    {
        hint = hint_for(&ws, attr, &context, before);
    }
    let hint_tokens = hint.as_deref().map_or(0, |h| tokens::estimate(h).value);

    attribution::record(
        attr,
        Delivery {
            id,
            program: context.program(),
            tokens_before: before,
            tokens_after: delivered,
            overhead: hint_tokens,
            cause: "hint",
            filtered_lines: out.filtered.as_ref().map_or(0, |f| f.removed_lines),
            kind: RecordKind::Run,
            cwd,
        },
    );

    let mut specific = serde_json::Map::new();
    specific.insert("hookEventName".into(), json!("PostToolUse"));
    if worth_it {
        // Start from the tool's own response so every field it carries
        // survives; only the text changes.
        let mut replaced = response.clone();
        let mut text = out.content.clone();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        replaced["stdout"] = json!(text);
        replaced["stderr"] = json!("");
        specific.insert("updatedToolOutput".into(), replaced);
    }
    if let Some(h) = hint {
        specific.insert("additionalContext".into(), json!(h));
    }
    if specific.len() == 1 {
        return Ok(None);
    }
    Ok(Some(
        json!({ "hookSpecificOutput": Value::Object(specific) }),
    ))
}

/// Is this command ttk itself (whose output is already counted)?
pub fn already_ttk(command: &str) -> bool {
    // Any stage: `cargo test | ttk compile` is recorded by `ttk compile`.
    command
        .split(['|', ';', '&', '\n'])
        .filter_map(|stage| stage.split_whitespace().next())
        .any(|w| {
            let w = w.trim_matches(|c| c == '"' || c == '\'' || c == '(');
            let base = w.rsplit(['/', '\\']).next().unwrap_or(w);
            matches!(base, "ttk" | "ttk.exe")
        })
}

/// The words of the command an output is most likely to belong to.
///
/// `cd x && FOO=1 cargo test 2>&1 | tail -n 30` belongs to `cargo test`:
/// the last `&&`/`;` segment that is not a `cd`, its first pipe stage, with
/// leading environment assignments and an `rtk` proxy prefix removed.
pub fn segment_words(command: &str) -> Vec<String> {
    let mut segments: Vec<&str> = Vec::new();
    let mut rest = command;
    while let Some(pos) = ["&&", "||", ";", "\n"]
        .iter()
        .filter_map(|sep| rest.find(sep).map(|p| (p, sep.len())))
        .min()
    {
        segments.push(&rest[..pos.0]);
        rest = &rest[pos.0 + pos.1..];
    }
    segments.push(rest);
    let chosen = segments
        .iter()
        .rev()
        .map(|s| s.trim())
        .find(|s| {
            !s.is_empty() && {
                let w = s.split_whitespace().next().unwrap_or("");
                !matches!(w, "cd" | "pushd" | "popd" | "set" | "export")
            }
        })
        .unwrap_or("");
    let stage = chosen.split('|').next().unwrap_or("").trim();
    let mut words: Vec<String> = stage
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect();
    while words
        .first()
        .is_some_and(|w| w.contains('=') && !w.starts_with('-'))
    {
        words.remove(0);
    }
    if words.first().is_some_and(|w| w == "rtk") {
        words.remove(0);
    }
    words
}

fn command_context(command: &str, cwd: &Path) -> CommandContext {
    CommandContext {
        argv: segment_words(command),
        cwd: cwd.display().to_string(),
        ..Default::default()
    }
}

/// One line asking the agent to teach a filter — once per command and
/// session, and only when no rule covers the command yet.
fn hint_for(
    ws: &Workspace,
    attr: &Attribution,
    context: &CommandContext,
    before: u64,
) -> Option<String> {
    let program = context.program()?;
    let scope = match context.subcommand() {
        Some(sub) => format!("{program} {sub}"),
        None => program.clone(),
    };
    let session = attr.session.clone().unwrap_or_default();
    let path = ws.root().join("hints.json");
    let mut state: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let seen = state
        .entry(session.clone())
        .or_insert_with(|| json!([]))
        .as_array_mut()?;
    if seen.iter().any(|v| v.as_str() == Some(scope.as_str())) {
        return None;
    }
    seen.push(json!(scope));
    // Keep the file small: the newest 50 sessions.
    while state.len() > 50 {
        let first = state.keys().next().cloned()?;
        state.remove(&first);
    }
    let _ = std::fs::write(&path, Value::Object(state).to_string());
    Some(format!(
        "ttk: `{scope}` printed ~{before} tokens and little was filtered. If part of it is \
         noise you will see again, teach a filter: `ttk learn --last` with that noise in \
         <filter-trash> (see ttk.md §4). Ignore if it was all relevant."
    ))
}

// ---------------------------------------------------------------------------
// SessionStart / SubagentStart
// ---------------------------------------------------------------------------

fn inject(
    input: &Value,
    cwd: &Path,
    config: &Config,
    attr: &Attribution,
    event: &str,
) -> Option<Value> {
    if !config.hooks.inject_context {
        return None;
    }
    let text = crate::guide::agent_context();
    let tokens = tokens::estimate(&text).value;
    let source = str_field(input, "source").unwrap_or_else(|| "subagent".to_string());
    // One injection per session/agent/source/second: Claude Code runs an
    // identical hook registered twice only once, but be safe.
    let id = format!(
        "ctx:{}:{}:{}:{}",
        attr.session.as_deref().unwrap_or("-"),
        attr.agent_id.as_deref().unwrap_or("main"),
        source,
        ttk_core::ids::now_millis() / 1000
    );
    attribution::record_overhead(attr, cwd, tokens, "context", Some(id));
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": text,
        }
    }))
}

// ---------------------------------------------------------------------------
// Stop / SubagentStop / SessionEnd
// ---------------------------------------------------------------------------

fn import_usage(config: &Config, path: Option<&Value>) {
    if !config.hooks.import_usage {
        return;
    }
    let Some(path) = path.and_then(Value::as_str).filter(|p| !p.is_empty()) else {
        return;
    };
    let Some(store) = ttk_store::ProviderStore::open() else {
        return;
    };
    let path = Path::new(path);
    // The main transcript and the subagent transcripts beside it
    // (`<session>/subagents/agent-*.jsonl`). A subagent's own file may not
    // exist yet when its stop event fires; the next stop picks it up.
    let mut files: Vec<PathBuf> = vec![path.to_path_buf()];
    let sub = path.with_extension("").join("subagents");
    if let Ok(entries) = std::fs::read_dir(&sub) {
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl")),
        );
    }
    for file in files.iter().filter(|f| f.is_file()) {
        if let Err(e) = store.import_file(file) {
            log_error(&format!("usage import of {}: {e}", file.display()));
        }
    }
}

fn log_error(msg: &str) {
    let Some(dir) = ttk_core::config::global_home() else {
        return;
    };
    let path = dir.join("hook.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{} {msg}", ttk_core::ids::now_millis());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_an_output_belongs_to_is_found() {
        assert_eq!(
            segment_words("cd /x && FOO=1 cargo test --all 2>&1 | tail -n 30"),
            ["cargo", "test", "--all", "2>&1"]
        );
        assert_eq!(segment_words("rtk git status"), ["git", "status"]);
        assert_eq!(segment_words("npm install; cd .."), ["npm", "install"]);
        assert!(already_ttk("ttk run -- cargo test"));
        assert!(already_ttk("cd x && ttk retrieve cap_1"));
        assert!(already_ttk("cargo test | ttk compile"));
        assert!(!already_ttk("cargo test --features ttk"));
    }
}
