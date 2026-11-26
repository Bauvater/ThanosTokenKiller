//! `ttk dashboard` — every statistic ttk keeps, in one local HTML page.
//!
//! The page (`assets/dashboard.html`) is compiled into the binary and filled
//! with one JSON document. It makes no network request, needs no server, and
//! is written to the global ttk folder, so it is as private as the ledger it
//! renders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use ttk_core::{Error, Result};
use ttk_store::{GlobalUsage, MeasuredUsage};

use crate::commands::Context;
use crate::ui::{self, outln};

const TEMPLATE: &str = include_str!("../assets/dashboard.html");
const PLACEHOLDER: &str = "/*__TTK_DATA__*/null";

pub fn run(ctx: &Context, out: Option<&str>, open: bool) -> Result<i32> {
    let data = collect(ctx)?;
    let html = render(&data)?;
    let path = match out {
        Some(p) => PathBuf::from(p),
        None => ttk_core::config::global_home()
            .ok_or_else(|| Error::other("no global ttk folder on this platform"))?
            .join("dashboard.html"),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, html)?;
    if ctx.json {
        return ctx.print_json(json!({"path": path.display().to_string()}));
    }
    outln!(
        "{}",
        ui::ok(format!("dashboard written to {}", path.display()))
    );
    if open {
        open_in_browser(&path);
    }
    Ok(0)
}

/// Fill the template. `<` is escaped so no value can close the script tag.
pub fn render(data: &Value) -> Result<String> {
    if !TEMPLATE.contains(PLACEHOLDER) {
        return Err(Error::other("dashboard template has no data placeholder"));
    }
    let json = serde_json::to_string(data)
        .map_err(Error::other)?
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(TEMPLATE.replacen(PLACEHOLDER, &json, 1))
}

fn day_string(day: i64) -> String {
    ui::iso_date(day)
}

/// Everything the page shows, as the documented JSON schema.
pub fn collect(ctx: &Context) -> Result<Value> {
    let ledger = ttk_store::UsageLedger::open()
        .ok_or_else(|| Error::other("the global usage ledger is switched off (TTK_USAGE=0)"))?;
    let usage = ledger.aggregate()?;
    let offset = ui::local_offset_secs();
    let daily = ledger.daily(offset, None)?;
    let measured = ttk_store::ProviderStore::open()
        .and_then(|s| s.aggregate().ok())
        .unwrap_or_default();
    let (config, loaded) = ctx.load()?;
    let cwd = std::env::current_dir()?;
    let rules = ttk_core::config::find_project_root(&cwd)
        .and_then(|root| ttk_learn::Layered::load(&root.join(ttk_store::WORKSPACE_DIR)).ok());
    Ok(build(
        &usage,
        &daily,
        &measured,
        rules.as_ref(),
        &config,
        &loaded,
        &cwd,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    usage: &GlobalUsage,
    daily: &BTreeMap<i64, ttk_store::usage::ProgramTotals>,
    measured: &MeasuredUsage,
    rules: Option<&ttk_learn::Layered>,
    config: &ttk_core::config::Config,
    loaded: &ttk_core::config::LoadedConfig,
    cwd: &Path,
) -> Value {
    let pct = |saved: u64, before: u64| {
        if before == 0 {
            0.0
        } else {
            saved as f64 * 100.0 / before as f64
        }
    };
    let totals = json!({
        "runs": usage.events,
        "passthrough_runs": usage.passthrough,
        "tokens_before": usage.tokens_before,
        "tokens_after": usage.tokens_after,
        "saved": usage.saved(),
        "saved_pct": usage.saved_percent(),
        "overhead": usage.overhead,
        "net": usage.net(),
        "method": if usage.method.is_empty() { "estimated" } else { usage.method.as_str() },
        "filtered_lines": usage.filtered_lines,
        "skipped_lines": usage.skipped_lines,
        "duplicates": usage.duplicates,
        "first_at": usage.first_millis,
        "last_at": usage.last_millis,
        "overhead_by_cause": usage.overhead_by_cause,
    });
    let m = &measured.totals;
    let measured_json = json!({
        "available": m.messages > 0,
        "messages": m.messages,
        "sessions": measured.sessions,
        "input": m.input,
        "output": m.output,
        "cache_read": m.cache_read,
        "cache_creation": m.cache_creation,
        "total": m.total(),
    });
    let daily_json: Vec<Value> = daily
        .iter()
        .map(|(d, t)| {
            json!({"day": day_string(*d), "runs": t.runs, "before": t.tokens_before,
                   "after": t.tokens_after, "saved": t.saved(), "overhead": t.overhead})
        })
        .collect();
    let projects: Vec<Value> = usage
        .projects
        .iter()
        .map(|p| {
            json!({"name": p.project, "id": p.project_id, "runs": p.events,
                   "before": p.tokens_before, "after": p.tokens_after, "saved": p.saved(),
                   "saved_pct": p.saved_percent(), "overhead": p.overhead,
                   "last_at": p.last_millis})
        })
        .collect();
    let programs: Vec<Value> = usage
        .by_program()
        .iter()
        .map(|(name, t)| {
            json!({"name": name, "runs": t.runs, "before": t.tokens_before,
                   "after": t.tokens_after, "saved": t.saved(),
                   "saved_pct": pct(t.saved(), t.tokens_before)})
        })
        .collect();
    let sessions: Vec<Value> = usage
        .sessions
        .iter()
        .map(|s| {
            let model = s
                .model
                .clone()
                .or_else(|| measured.session_model.get(&s.id).cloned());
            json!({"id": s.id, "project": s.project, "agent": s.agent, "model": model,
                   "runs": s.totals.runs, "subagents": s.subagents,
                   "before": s.totals.tokens_before, "after": s.totals.tokens_after,
                   "saved": s.totals.saved(), "overhead": s.totals.overhead,
                   "measured_total": measured.by_session.get(&s.id).map(|t| t.total()),
                   "first_at": s.first_millis, "last_at": s.last_millis})
        })
        .collect();
    let agents: Vec<Value> = usage
        .by_agent
        .iter()
        .map(|(name, t)| {
            json!({"name": name, "runs": t.runs, "before": t.tokens_before,
                   "after": t.tokens_after, "saved": t.saved(), "overhead": t.overhead,
                   "measured_total": measured.by_agent.get(name).map(|m| m.total())})
        })
        .collect();

    // Models: ttk's savings joined through the session, measured usage as is.
    let mut model_saved: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for s in &usage.sessions {
        let model = s
            .model
            .clone()
            .or_else(|| measured.session_model.get(&s.id).cloned())
            .unwrap_or_else(|| "unknown".to_string());
        let e = model_saved.entry(model).or_default();
        e.0 += s.totals.runs;
        e.1 += s.totals.saved();
    }
    let mut model_names: Vec<String> = measured.by_model.keys().cloned().collect();
    for k in model_saved.keys() {
        if !model_names.contains(k) {
            model_names.push(k.clone());
        }
    }
    let models: Vec<Value> = model_names
        .iter()
        .map(|name| {
            let mt = measured.by_model.get(name).copied().unwrap_or_default();
            let (runs, saved) = model_saved.get(name).copied().unwrap_or_default();
            json!({"model": name, "runs": runs, "saved": saved, "messages": mt.messages,
                   "input": mt.input, "output": mt.output, "cache_read": mt.cache_read,
                   "cache_creation": mt.cache_creation})
        })
        .collect();

    let filters = rules.map(filters_json).unwrap_or_default();

    let home = ttk_core::config::global_home();
    let project_settings =
        ttk_core::config::find_project_root(cwd).map(|r| r.join(".claude").join("settings.json"));
    let global_settings = crate::install::Agent::ClaudeCode
        .global_path()
        .map(|p| crate::claude_hooks::settings_path_for(&p, true));
    let hooks_installed = project_settings
        .iter()
        .chain(global_settings.iter())
        .any(|p| crate::claude_hooks::installed(p));
    let mut settings = serde_json::to_value(config).unwrap_or(json!({}));
    if let Some(obj) = settings.as_object_mut() {
        obj.insert(
            "ledger_path".into(),
            json!(usage.path.as_ref().map(|p| p.display().to_string())),
        );
        obj.insert(
            "global_dir".into(),
            json!(home.as_ref().map(|p| p.display().to_string())),
        );
        obj.insert(
            "config_files".into(),
            json!(
                loaded
                    .layers
                    .iter()
                    .filter_map(|o| match o {
                        ttk_core::config::ConfigOrigin::UserFile(p)
                        | ttk_core::config::ConfigOrigin::ProjectFile(p)
                        | ttk_core::config::ConfigOrigin::SessionFile(p) => {
                            Some(p.display().to_string())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            ),
        );
        obj.insert("hooks_installed".into(), json!(hooks_installed));
    }

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "generated_at": ttk_core::ids::now_millis(),
        "totals": totals,
        "measured": measured_json,
        "daily": daily_json,
        "projects": projects,
        "programs": programs,
        "sessions": sessions,
        "agents": agents,
        "models": models,
        "filters": filters,
        "settings": settings,
    })
}

fn filters_json(layered: &ttk_learn::Layered) -> Vec<Value> {
    let now = ttk_core::ids::now_millis();
    let findings = crate::learncmd::audit_findings(layered, now);
    layered
        .merged
        .rules
        .iter()
        .map(|r| {
            let mine: Vec<String> = findings
                .iter()
                .filter(|f| f.rule == r.id)
                .map(|f| f.summary())
                .collect();
            let status = crate::learncmd::rule_status(r, &findings);
            json!({
                "id": r.id,
                "scope": r.scope.to_string(),
                "pattern": r.pattern,
                "action": r.action.as_str(),
                "origin": layered.origin_of(&r.id).map(|o| o.as_str()),
                "enabled": r.enabled,
                "when": r.when.as_str(),
                "hits": r.hits,
                "lines_removed": r.lines_removed,
                "tokens_saved": r.tokens_saved,
                "teach_tokens": r.teach_tokens,
                "net": r.net_tokens(),
                "created_at": r.created_millis,
                "last_hit_at": r.last_hit_millis,
                "status": status,
                "findings": mine,
                "now": now,
            })
        })
        .collect()
}

fn open_in_browser(path: &Path) {
    let p = path.display().to_string();
    let result = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &p])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&p).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&p).spawn()
    };
    if result.is_err() {
        outln!("{}", ui::detail("open it in your browser"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_cannot_close_the_script_tag() {
        let html = render(&json!({"x": "</script><b>"})).expect("render");
        assert!(!html.contains("</script><b>"));
        assert!(html.contains("\\u003c/script>"));
        assert!(!html.contains(PLACEHOLDER));
    }
}
