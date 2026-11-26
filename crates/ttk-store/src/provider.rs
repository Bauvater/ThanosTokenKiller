//! Measured model usage, imported from agent transcripts.
//!
//! ttk's own numbers are *counterfactual*: "this output would have cost ~N
//! tokens, it cost ~M instead". Nothing can measure tokens that never reached
//! a model, so those stay estimates and are labelled as such.
//!
//! What *can* be measured is what the model actually consumed. Claude Code
//! writes the provider's `usage` block of every response into its transcript:
//!
//! ```text
//! ~/.claude/projects/<project>/<session>.jsonl                 the main agent
//! ~/.claude/projects/<project>/<session>/subagents/agent-*.jsonl  subagents
//! ```
//!
//! This module reads those files and keeps one record per *response* in
//! `<global>/provider.jsonl`. Two traps are handled explicitly:
//!
//! * **One response, several lines.** A streamed response is written as one
//!   line per content block, every line carrying the same `message.id` and the
//!   same usage. Summing lines would double or triple every number, so records
//!   are keyed by `message.id` and counted once.
//! * **Re-import.** A hook and `ttk usage sync` may read the same transcript
//!   any number of times. Each file is read from where the last import
//!   stopped, and the id key makes any overlap harmless anyway.
//!
//! The transcript format is internal to Claude Code and may change; every
//! field is read defensively and a line that does not parse is skipped.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use ttk_core::{Error, Result};

pub const PROVIDER_FILE: &str = "provider.jsonl";
const SYNC_STATE_FILE: &str = "provider-sync.json";
const SUMMARY_FILE: &str = "provider-summary.json";

#[derive(Serialize, Deserialize)]
struct CachedSummary {
    key: String,
    usage: MeasuredUsage,
}

/// One model response, as the provider reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// The provider's message id: the de-duplication key.
    pub id: String,
    pub at_millis: u64,
    pub session: String,
    /// Set for subagent responses.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_type: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl ProviderRecord {
    /// Every input token the model processed for this response, cached or not.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation
    }

    pub fn total(&self) -> u64 {
        self.context() + self.output
    }
}

/// Measured totals for one group (a model, a session, everything).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasuredTotals {
    pub messages: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl MeasuredTotals {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    fn absorb(&mut self, r: &ProviderRecord) {
        self.messages += 1;
        self.input += r.input;
        self.output += r.output;
        self.cache_read += r.cache_read;
        self.cache_creation += r.cache_creation;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeasuredUsage {
    pub totals: MeasuredTotals,
    pub by_model: BTreeMap<String, MeasuredTotals>,
    pub by_session: BTreeMap<String, MeasuredTotals>,
    /// The model a session used most, by responses.
    pub session_model: BTreeMap<String, String>,
    /// `main` or `subagent:<type>`.
    pub by_agent: BTreeMap<String, MeasuredTotals>,
    pub sessions: u64,
    pub duplicates: u64,
}

/// The measured-usage store in the global ttk folder.
pub struct ProviderStore {
    dir: PathBuf,
}

/// What one import did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportStats {
    pub files: u64,
    pub records: u64,
    pub skipped_lines: u64,
}

impl ProviderStore {
    pub fn open() -> Option<Self> {
        if matches!(
            std::env::var("TTK_USAGE").as_deref(),
            Ok("0") | Ok("off") | Ok("false") | Ok("no")
        ) {
            return None;
        }
        Some(Self::at(ttk_core::config::global_home()?))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(PROVIDER_FILE)
    }

    fn lock(&self) -> Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("provider.lock"))?;
        f.lock()
            .map_err(|e| Error::storage(format!("cannot lock provider store: {e}")))?;
        Ok(f)
    }

    /// Import the new part of one transcript.
    pub fn import_file(&self, transcript: &Path) -> Result<ImportStats> {
        let _guard = self.lock()?;
        let mut state = self.load_state();
        let stats = self.import_locked(transcript, &mut state)?;
        self.save_state(&state)?;
        Ok(stats)
    }

    /// Import every transcript under `root` (`~/.claude/projects`), and the
    /// subagent transcripts beside them.
    pub fn import_tree(&self, root: &Path) -> Result<ImportStats> {
        let _guard = self.lock()?;
        let mut state = self.load_state();
        let mut total = ImportStats::default();
        for file in transcripts_under(root) {
            match self.import_locked(&file, &mut state) {
                Ok(s) => {
                    total.files += s.files;
                    total.records += s.records;
                    total.skipped_lines += s.skipped_lines;
                }
                Err(_) => total.skipped_lines += 1,
            }
        }
        self.save_state(&state)?;
        Ok(total)
    }

    fn import_locked(
        &self,
        transcript: &Path,
        state: &mut BTreeMap<String, u64>,
    ) -> Result<ImportStats> {
        let key = transcript.display().to_string();
        let len = std::fs::metadata(transcript)?.len();
        let mut offset = state.get(&key).copied().unwrap_or(0);
        if offset > len {
            // Truncated or replaced: read it again; ids keep it correct.
            offset = 0;
        }
        if offset == len {
            return Ok(ImportStats::default());
        }
        let mut f = File::open(transcript)?;
        f.seek(SeekFrom::Start(offset))?;
        // Streamed line by line: transcripts run to hundreds of megabytes,
        // and almost every line is a tool result nobody here needs.
        let mut reader = BufReader::with_capacity(1 << 20, f.take(len - offset));
        let mut line = Vec::new();
        let mut complete: u64 = 0;

        let agent_type = subagent_type(transcript);
        let mut seen = HashSet::new();
        let mut out = String::new();
        let mut stats = ImportStats {
            files: 1,
            ..ImportStats::default()
        };
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            // Only whole lines: one still being written is read next time.
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            complete += n as u64;
            // A cheap byte test before any parsing: only assistant responses
            // carry usage, and they are a small share of the lines.
            if !contains(&line, b"\"usage\"") || !contains(&line, b"\"assistant\"") {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                stats.skipped_lines += 1;
                continue;
            };
            match parse_line(text, agent_type.as_deref()) {
                Some(rec) => {
                    if seen.insert(rec.id.clone()) {
                        out.push_str(&serde_json::to_string(&rec).map_err(Error::other)?);
                        out.push('\n');
                        stats.records += 1;
                    }
                }
                None => stats.skipped_lines += 1,
            }
        }
        if !out.is_empty() {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path())?;
            f.write_all(out.as_bytes())?;
        }
        state.insert(key, offset + complete);
        Ok(stats)
    }

    fn load_state(&self) -> BTreeMap<String, u64> {
        std::fs::read_to_string(self.dir.join(SYNC_STATE_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &BTreeMap<String, u64>) -> Result<()> {
        let path = self.dir.join(SYNC_STATE_FILE);
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string(state).map_err(Error::other)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Every record, each response once.
    pub fn read(&self) -> Result<(Vec<ProviderRecord>, u64)> {
        let file = match File::open(self.path()) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
            Err(e) => return Err(e.into()),
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut duplicates = 0;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let Ok(rec) = serde_json::from_str::<ProviderRecord>(&line) else {
                continue;
            };
            if seen.insert(rec.id.clone()) {
                out.push(rec);
            } else {
                duplicates += 1;
            }
        }
        Ok((out, duplicates))
    }

    /// Totals of every record, each response once.
    ///
    /// Folding the whole store means reading every record, which grows with
    /// every session; reports ask for it far more often than it changes. So
    /// the result is cached beside the store, keyed by the store's size and
    /// modification time, and recomputed only when either moved.
    pub fn aggregate(&self) -> Result<MeasuredUsage> {
        let key = std::fs::metadata(self.path()).ok().map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            format!("{}:{mtime}", m.len())
        });
        let Some(key) = key else {
            return Ok(MeasuredUsage::default());
        };
        let cache = self.dir.join(SUMMARY_FILE);
        if let Some(hit) = std::fs::read_to_string(&cache)
            .ok()
            .and_then(|t| serde_json::from_str::<CachedSummary>(&t).ok())
            .filter(|c| c.key == key)
        {
            return Ok(hit.usage);
        }
        let (records, duplicates) = self.read()?;
        let usage = fold(&records, duplicates);
        let tmp = cache.with_extension(format!("{}.tmp", std::process::id()));
        if let Ok(text) = serde_json::to_string(&CachedSummary {
            key,
            usage: usage.clone(),
        }) && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &cache);
        }
        Ok(usage)
    }

    /// Forget every measured record (`ttk usage forget --all`).
    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock()?;
        for name in [PROVIDER_FILE, SYNC_STATE_FILE, SUMMARY_FILE] {
            match std::fs::remove_file(self.dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

pub fn fold(records: &[ProviderRecord], duplicates: u64) -> MeasuredUsage {
    let mut usage = MeasuredUsage {
        duplicates,
        ..MeasuredUsage::default()
    };
    let mut model_votes: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for r in records {
        usage.totals.absorb(r);
        usage.by_model.entry(r.model.clone()).or_default().absorb(r);
        usage
            .by_session
            .entry(r.session.clone())
            .or_default()
            .absorb(r);
        let bucket = match (&r.agent_id, &r.agent_type) {
            (Some(_), Some(t)) => format!("subagent:{t}"),
            (Some(_), None) => "subagent".to_string(),
            _ => "main".to_string(),
        };
        usage.by_agent.entry(bucket).or_default().absorb(r);
        if r.agent_id.is_none() {
            *model_votes
                .entry(r.session.clone())
                .or_default()
                .entry(r.model.clone())
                .or_default() += 1;
        }
    }
    usage.sessions = usage.by_session.len() as u64;
    usage.session_model = model_votes
        .into_iter()
        .filter_map(|(s, votes)| {
            votes
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .map(|(m, _)| (s, m))
        })
        .collect();
    usage
}

/// Parse one transcript line into a record, or `None` if it is not an
/// assistant response with usage.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// Only the fields that matter: serde skips the (large) message content
// without building it, which is most of the speed of an import.
#[derive(Deserialize)]
struct TLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "agentId")]
    agent_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<TMessage>,
}

#[derive(Deserialize)]
struct TMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<TUsage>,
}

#[derive(Deserialize, Default)]
struct TUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

pub fn parse_line(line: &str, agent_type: Option<&str>) -> Option<ProviderRecord> {
    let v: TLine = serde_json::from_str(line).ok()?;
    if v.kind.as_deref()? != "assistant" {
        return None;
    }
    let msg = v.message?;
    let usage = msg.usage?;
    let model = msg.model?;
    // Synthetic placeholders (`<synthetic>`) carry zero usage and no model.
    if model.starts_with('<') {
        return None;
    }
    let agent_id = v.agent_id.filter(|s| !s.is_empty());
    Some(ProviderRecord {
        id: msg.id?,
        at_millis: v
            .timestamp
            .as_deref()
            .and_then(parse_rfc3339_millis)
            .unwrap_or(0),
        session: v.session_id?,
        agent_type: agent_id.as_ref().and(agent_type.map(String::from)),
        agent_id,
        model,
        project: v.cwd,
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_creation: usage.cache_creation_input_tokens,
    })
}

/// `agent-<id>.jsonl` has an `agent-<id>.meta.json` beside it naming its type.
fn subagent_type(transcript: &Path) -> Option<String> {
    let name = transcript.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    if !stem.starts_with("agent-") {
        return None;
    }
    let meta = transcript.with_file_name(format!("{stem}.meta.json"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    v.get("agentType")?.as_str().map(String::from)
}

/// Every `*.jsonl` transcript below `root`, subagents included.
pub fn transcripts_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Where Claude Code keeps its transcripts (`CLAUDE_CONFIG_DIR` honoured).
pub fn claude_projects_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_CONFIG_DIR")
        && !p.trim().is_empty()
    {
        return Some(PathBuf::from(p).join("projects"));
    }
    dirs_home().map(|h| h.join(".claude").join("projects"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `2026-11-25T17:00:44.426Z` → unix millis. Only the UTC `Z` form Claude
/// Code writes; anything else is `None` rather than a wrong number.
pub fn parse_rfc3339_millis(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let millis: i64 = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs * 1000 + millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, session: &str, agent: Option<&str>, out: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "sessionId": session,
            "agentId": agent,
            "timestamp": "2026-11-25T17:00:44.426Z",
            "cwd": "C:\\p",
            "message": {"id": id, "model": "claude-opus-5-5",
                "usage": {"input_tokens": 2, "output_tokens": out,
                          "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10}}
        })
        .to_string()
    }

    #[test]
    fn timestamps_parse_exactly() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339_millis("2026-11-25T17:00:44.426Z"),
            Some(1_795_626_044_426)
        );
        assert_eq!(parse_rfc3339_millis("2026-11-25T17:00:44+01:00"), None);
    }

    #[test]
    fn a_split_response_counts_once_and_reimport_adds_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        let t = dir.path().join("s1.jsonl");
        let body = [
            line("msg_1", "s1", None, 50),
            line("msg_1", "s1", None, 50),
            line("msg_1", "s1", None, 50),
            "{\"type\":\"user\",\"message\":{}}".to_string(),
            line("msg_2", "s1", None, 5),
        ]
        .join("\n")
            + "\n";
        std::fs::write(&t, body).expect("write");
        let store = ProviderStore::at(dir.path().join("global"));
        assert_eq!(store.import_file(&t).expect("import").records, 2);
        assert_eq!(store.import_file(&t).expect("again").records, 0);
        // Even a forced full re-read cannot double count.
        std::fs::remove_file(dir.path().join("global").join(SYNC_STATE_FILE)).expect("rm");
        store.import_file(&t).expect("forced");
        let usage = store.aggregate().expect("aggregate");
        assert_eq!(usage.totals.messages, 2);
        assert_eq!(usage.totals.output, 55);
        assert_eq!(usage.totals.cache_read, 200);
        assert_eq!(usage.session_model["s1"], "claude-opus-5-5");
    }

    #[test]
    fn a_half_written_line_waits_for_the_next_import() {
        let dir = tempfile::tempdir().expect("tmp");
        let t = dir.path().join("s1.jsonl");
        let full = line("msg_1", "s1", None, 7);
        std::fs::write(&t, format!("{}\n{}", full, &full[..20])).expect("write");
        let store = ProviderStore::at(dir.path().join("g"));
        assert_eq!(store.import_file(&t).expect("import").records, 1);
        std::fs::write(&t, format!("{full}\n{}\n", line("msg_2", "s1", None, 1))).expect("w");
        assert_eq!(store.import_file(&t).expect("import").records, 1);
        assert_eq!(store.aggregate().expect("agg").totals.messages, 2);
    }

    #[test]
    fn subagents_are_attributed_by_their_meta_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let sub = dir.path().join("proj").join("s1").join("subagents");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(
            sub.join("agent-a1.jsonl"),
            format!("{}\n", line("msg_9", "s1", Some("a1"), 3)),
        )
        .expect("w");
        std::fs::write(
            sub.join("agent-a1.meta.json"),
            "{\"agentType\":\"Explore\"}",
        )
        .expect("w");
        std::fs::write(
            dir.path().join("proj").join("s1.jsonl"),
            format!("{}\n", line("msg_1", "s1", None, 3)),
        )
        .expect("w");
        let store = ProviderStore::at(dir.path().join("g"));
        let stats = store.import_tree(&dir.path().join("proj")).expect("tree");
        assert_eq!(stats.files, 2);
        let usage = store.aggregate().expect("agg");
        assert_eq!(usage.by_agent["subagent:Explore"].messages, 1);
        assert_eq!(usage.by_agent["main"].messages, 1);
    }
}
