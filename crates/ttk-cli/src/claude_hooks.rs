//! Registering `ttk hook` in Claude Code's `settings.json`.
//!
//! The settings file belongs to the user: it holds their permissions, their
//! other hooks, their model choice. So this module touches exactly one thing —
//! hook entries whose command runs `ttk … hook` — and preserves every other key
//! and every other hook, in order. Installing twice changes nothing; a moved
//! binary updates the command in place.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use ttk_core::{Error, Result};

/// The events ttk registers for, with their matcher and timeout (seconds).
pub const EVENTS: [(&str, Option<&str>, u64); 7] = [
    ("PostToolUse", Some("Bash|PowerShell"), 30),
    ("SessionStart", None, 10),
    ("UserPromptSubmit", None, 10),
    ("SubagentStart", None, 10),
    ("Stop", None, 15),
    ("SubagentStop", None, 15),
    ("SessionEnd", None, 15),
];

/// What installing did to one settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    Created,
    Updated,
    Unchanged,
    Removed,
}

impl HookAction {
    pub fn as_str(self) -> &'static str {
        match self {
            HookAction::Created => "hooks added",
            HookAction::Updated => "hooks updated",
            HookAction::Unchanged => "hooks up to date",
            HookAction::Removed => "hooks removed",
        }
    }
}

/// `settings.json` for an instruction file: `<dir of CLAUDE.md>/.claude/` for
/// a project, the directory itself for the global `~/.claude/CLAUDE.md`.
pub fn settings_path_for(instruction_file: &Path, global: bool) -> PathBuf {
    let dir = instruction_file.parent().unwrap_or(Path::new("."));
    if global {
        dir.join("settings.json")
    } else {
        dir.join(".claude").join("settings.json")
    }
}

/// The command Claude Code runs: this binary, by absolute path, so a hook
/// works even where `ttk` is not on the PATH the agent's shell sees.
pub fn hook_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .map(|p| {
            let s = p.display().to_string();
            // `\\?\C:\…` from canonicalize is not something a shell runs.
            s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/")
        })
        .unwrap_or_else(|| "ttk".to_string());
    format!("\"{exe}\" hook")
}

/// Is this hook command one of ttk's?
pub fn is_ttk_command(cmd: &str) -> bool {
    let c = cmd.replace('\\', "/").to_ascii_lowercase();
    let c = c.trim();
    (c.contains("/ttk\" hook") || c.contains("/ttk.exe\" hook") || c.starts_with("ttk hook"))
        || c.ends_with("ttk.exe hook")
        || c.ends_with("/ttk hook")
}

/// Insert or refresh ttk's hooks in a settings document.
pub fn upsert(settings: &mut Map<String, Value>, command: &str) {
    let hooks = settings
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("object");
    remove_from(hooks);
    for (event, matcher, timeout) in EVENTS {
        let mut group = Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json!(m));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": command, "timeout": timeout }]),
        );
        let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        if let Some(arr) = list.as_array_mut() {
            arr.push(Value::Object(group));
        }
    }
}

/// Take every ttk hook out, leaving the rest exactly as it was.
pub fn remove_from(hooks: &mut Map<String, Value>) -> usize {
    let mut removed = 0;
    for (_, list) in hooks.iter_mut() {
        let Some(groups) = list.as_array_mut() else {
            continue;
        };
        for g in groups.iter_mut() {
            if let Some(inner) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = inner.len();
                inner.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_ttk_command)
                });
                removed += before - inner.len();
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|a| !a.is_empty())
        });
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    removed
}

fn load(path: &Path) -> Result<Option<Map<String, Value>>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Some(Map::new())),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => Ok(Some(m)),
            // Never overwrite a settings file we cannot read: that would
            // destroy the user's configuration.
            _ => Err(Error::Config(format!(
                "{} is not a JSON object; fix it or add the hooks by hand",
                path.display()
            ))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save(path: &Path, settings: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(settings.clone()))
        .map_err(Error::other)?
        + "\n";
    let tmp = path.with_extension("json.ttk-tmp");
    std::fs::write(&tmp, &text)?;
    if std::fs::rename(&tmp, path).is_err() {
        std::fs::write(path, &text)?;
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(())
}

/// Install ttk's hooks into one settings file.
pub fn install(path: &Path, command: &str, dry_run: bool) -> Result<HookAction> {
    let existing = load(path)?;
    let existed = existing.is_some();
    let mut settings = existing.clone().unwrap_or_default();
    upsert(&mut settings, command);
    let action = match existing {
        None => HookAction::Created,
        Some(old) if old == settings => HookAction::Unchanged,
        Some(_) => HookAction::Updated,
    };
    if !dry_run && action != HookAction::Unchanged {
        save(path, &settings)?;
    }
    let _ = existed;
    Ok(action)
}

/// Remove ttk's hooks from one settings file.
pub fn uninstall(path: &Path, dry_run: bool) -> Result<HookAction> {
    let Some(mut settings) = load(path)? else {
        return Ok(HookAction::Unchanged);
    };
    let removed = settings
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .map_or(0, remove_from);
    if settings
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        settings.remove("hooks");
    }
    if removed == 0 {
        return Ok(HookAction::Unchanged);
    }
    if !dry_run {
        save(path, &settings)?;
    }
    Ok(HookAction::Removed)
}

/// Are ttk's hooks registered in this file?
pub fn installed(path: &Path) -> bool {
    let Ok(Some(settings)) = load(path) else {
        return false;
    };
    settings
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .any(is_ttk_command)
}

/// Other PreToolUse/PostToolUse hooks that rewrite shell commands or their
/// output (rtk, context-mode, …). They may conflict with ttk's.
pub fn competing_hooks(path: &Path) -> Vec<String> {
    let Ok(Some(settings)) = load(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for event in ["PreToolUse", "PostToolUse"] {
        for g in hooks
            .get(event)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let matcher = g.get("matcher").and_then(Value::as_str).unwrap_or("*");
            if !(matcher.contains("Bash") || matcher == "*" || matcher.is_empty()) {
                continue;
            }
            for h in g
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if is_ttk_command(cmd) {
                    continue;
                }
                let lower = cmd.to_ascii_lowercase();
                if lower.contains("rtk") || lower.contains("context-mode") {
                    out.push(format!("{event}: {cmd}"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "\"C:/tools/ttk.exe\" hook";

    #[test]
    fn other_hooks_and_keys_survive_and_install_is_idempotent() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
        )
        .expect("write");
        assert_eq!(install(&path, CMD, false).expect("1"), HookAction::Updated);
        assert_eq!(
            install(&path, CMD, false).expect("2"),
            HookAction::Unchanged
        );
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("r")).expect("j");
        assert_eq!(v["model"], "opus");
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "rtk hook claude"
        );
        assert_eq!(v["hooks"]["PostToolUse"][0]["matcher"], "Bash|PowerShell");
        assert!(installed(&path));
        assert_eq!(competing_hooks(&path), ["PreToolUse: rtk hook claude"]);
    }

    #[test]
    fn a_moved_binary_replaces_the_old_command() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("settings.json");
        install(&path, "\"C:/old/ttk.exe\" hook", false).expect("old");
        install(&path, CMD, false).expect("new");
        let text = std::fs::read_to_string(&path).expect("r");
        assert!(!text.contains("C:/old"));
        assert_eq!(text.matches("ttk.exe\\\" hook").count(), EVENTS.len());
    }

    #[test]
    fn uninstall_leaves_only_what_was_there() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("w");
        install(&path, CMD, false).expect("install");
        assert_eq!(uninstall(&path, false).expect("rm"), HookAction::Removed);
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("r")).expect("j");
        assert_eq!(v, json!({"theme": "dark"}));
        assert!(!installed(&path));
    }

    #[test]
    fn an_unreadable_settings_file_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").expect("w");
        assert!(install(&path, CMD, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("r"), "{ not json");
    }

    #[test]
    fn settings_paths() {
        assert_eq!(
            settings_path_for(Path::new("/p/CLAUDE.md"), false),
            Path::new("/p/.claude/settings.json")
        );
        assert_eq!(
            settings_path_for(Path::new("/h/.claude/CLAUDE.md"), true),
            Path::new("/h/.claude/settings.json")
        );
    }
}
