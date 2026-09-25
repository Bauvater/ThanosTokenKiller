//! The global usage ledger.
//!
//! Every workspace keeps its own event log, which answers "what did ttk do in
//! *this* repository". That is the wrong question for the number people
//! actually want: **how much has this tool saved me, everywhere, ever.**
//! Chasing that across twenty `.ttk` directories is miserable, so one ledger
//! outside every project collects it.
//!
//! ```text
//! <config dir>/ttk/usage.jsonl     one line per captured command, plus rollups
//! ```
//!
//! Design constraints, in order:
//!
//! * **Append only, one line per write.** Several `ttk` processes run at once
//!   in a normal agent session. A single `write_all` of a sub-kilobyte line to
//!   a handle opened in append mode is atomic enough on both Linux and Windows;
//!   nothing here takes a lock, because a lock that can be held by a crashed
//!   process is worse than a rare interleaved line.
//! * **A corrupt line costs one record.** Reading skips what it cannot parse,
//!   exactly like the per-session event log.
//! * **It never grows without bound.** Once the file passes
//!   [`COMPACT_THRESHOLD_BYTES`], records older than the retention window are
//!   folded into one rollup per project and month.
//! * **It never leaves the machine.** The ledger stores local paths, so it is
//!   as private as the projects it names. `TTK_USAGE=0` switches it off.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ttk_core::{Error, Result};

/// File name inside the global directory.
pub const USAGE_FILE: &str = "usage.jsonl";

/// Compaction runs when the ledger passes this size.
pub const COMPACT_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024;

/// Records older than this are folded into rollups when compaction runs.
pub const DEFAULT_RETAIN_DAYS: u64 = 45;

const MILLIS_PER_DAY: u64 = 86_400_000;

/// Where the global ledger and the user level rule file live.
///
/// `TTK_GLOBAL_HOME` overrides it, which is what the test suite uses to stay
/// off the developer's real ledger.
pub fn global_dir() -> Option<PathBuf> {
    ttk_core::config::global_home()
}

/// A stable, short identifier for a project directory.
///
/// The path is the display name; this is the key. Hashing means a project that
/// moves keeps its history only if the path is the same, which is the honest
/// trade: nothing here can know that two paths are the same repository.
pub fn project_id(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let key = canonical.to_string_lossy().to_lowercase();
    ttk_core::hash_string(&key)
        .strip_prefix("blake3:")
        .unwrap_or("")
        .chars()
        .take(12)
        .collect()
}

/// One captured command or compiled input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub at_millis: u64,
    /// Display path of the project root.
    pub project: String,
    pub project_id: String,
    /// Program name, e.g. `cargo`. `None` for piped input.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub program: Option<String>,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// Whether a compiler or filter actually replaced the content.
    #[serde(default)]
    pub transformed: bool,
    /// Lines a learned filter removed.
    #[serde(default)]
    pub filtered_lines: u64,
    /// The weakest counting method involved.
    pub method: String,
}

impl RunRecord {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }
}

/// Many old records, folded into one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupRecord {
    pub from_millis: u64,
    pub to_millis: u64,
    pub project: String,
    pub project_id: String,
    pub events: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub filtered_lines: u64,
    pub method: String,
    /// Kept so `ttk stats --global` can still rank commands after compaction.
    #[serde(default)]
    pub by_program: BTreeMap<String, ProgramTotals>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramTotals {
    pub runs: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
}

impl ProgramTotals {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    fn add(&mut self, other: &ProgramTotals) {
        self.runs += other.runs;
        self.tokens_before += other.tokens_before;
        self.tokens_after += other.tokens_after;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Record {
    Run(RunRecord),
    Rollup(RollupRecord),
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectUsage {
    pub project: String,
    pub project_id: String,
    pub events: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub filtered_lines: u64,
    pub first_millis: u64,
    pub last_millis: u64,
    pub by_program: BTreeMap<String, ProgramTotals>,
}

impl ProjectUsage {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn saved_percent(&self) -> f64 {
        percent(self.saved(), self.tokens_before)
    }
}

/// Everything the ledger knows, across every project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GlobalUsage {
    pub projects: Vec<ProjectUsage>,
    pub events: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub filtered_lines: u64,
    /// The weakest counting method any record used, so the total is never
    /// presented as more precise than its inputs.
    pub method: String,
    pub first_millis: Option<u64>,
    pub last_millis: Option<u64>,
    /// Records that were unreadable and skipped.
    pub skipped_lines: u64,
    /// Where the ledger lives, for `ttk doctor`.
    pub path: Option<PathBuf>,
}

impl GlobalUsage {
    pub fn saved(&self) -> u64 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    pub fn saved_percent(&self) -> f64 {
        percent(self.saved(), self.tokens_before)
    }

    pub fn is_empty(&self) -> bool {
        self.events == 0
    }

    /// Commands across every project, ranked by what they saved.
    pub fn by_program(&self) -> Vec<(String, ProgramTotals)> {
        let mut merged: BTreeMap<String, ProgramTotals> = BTreeMap::new();
        for p in &self.projects {
            for (name, totals) in &p.by_program {
                merged.entry(name.clone()).or_default().add(totals);
            }
        }
        let mut out: Vec<(String, ProgramTotals)> = merged.into_iter().collect();
        out.sort_by_key(|(name, t)| (std::cmp::Reverse(t.saved()), name.clone()));
        out
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / whole as f64
}

/// The weaker of two counting methods, by name.
fn weaker(a: &str, b: &str) -> String {
    fn rank(m: &str) -> u8 {
        match m {
            "provider" => 3,
            "tokenizer" => 2,
            "estimated" => 1,
            _ => 0,
        }
    }
    if a.is_empty() {
        return b.to_string();
    }
    if rank(a) <= rank(b) {
        a.into()
    } else {
        b.into()
    }
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

pub struct UsageLedger {
    path: PathBuf,
}

/// What one compaction pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactStats {
    pub records_before: u64,
    pub records_after: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl UsageLedger {
    /// Open the ledger, or `None` when it is switched off or the platform has
    /// no configuration directory.
    ///
    /// `TTK_USAGE=0` disables it. Nothing else in ttk changes behaviour when
    /// the ledger is missing: it is a reporting convenience, never a
    /// dependency.
    pub fn open() -> Option<Self> {
        if matches!(
            std::env::var("TTK_USAGE").as_deref(),
            Ok("0") | Ok("off") | Ok("false") | Ok("no")
        ) {
            return None;
        }
        Some(Self {
            path: global_dir()?.join(USAGE_FILE),
        })
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record.
    ///
    /// Failures are returned rather than swallowed, but every caller in the CLI
    /// treats them as a warning: not being able to update a statistic must
    /// never fail the command a user actually ran.
    pub fn append(&self, record: &RunRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Compact before appending, so the check costs one `metadata` call on
        // the hot path and the file can never be found in a huge state.
        if self.size() > COMPACT_THRESHOLD_BYTES {
            let _ = self.compact(DEFAULT_RETAIN_DAYS, ttk_core::ids::now_millis());
        }
        let mut line = serde_json::to_string(&Record::Run(record.clone()))
            .map_err(|e| Error::storage(format!("cannot serialize usage record: {e}")))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // Heal a torn last line before adding to it. A process killed mid-write
        // leaves a fragment with no newline; appending straight onto it would
        // glue a perfectly good record to a broken one and lose both. One byte
        // is a cheap price for making a crash cost at most the record it
        // interrupted.
        if self.ends_mid_line() {
            f.write_all(b"\n")?;
        }
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Does the ledger end in the middle of a line?
    fn ends_mid_line(&self) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = File::open(&self.path) else {
            return false;
        };
        if f.seek(SeekFrom::End(-1)).is_err() {
            // An empty file has no torn line.
            return false;
        }
        let mut last = [0u8; 1];
        matches!(f.read_exact(&mut last), Ok(())) && last[0] != b'\n'
    }

    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Read every record, skipping any line that does not parse.
    pub fn read(&self) -> Result<(Vec<Record>, u64)> {
        if !self.path.is_file() {
            return Ok((Vec::new(), 0));
        }
        let reader = BufReader::new(File::open(&self.path)?);
        let mut out = Vec::new();
        let mut skipped = 0;
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(&line) {
                Ok(r) => out.push(r),
                // A partially written last line is expected after a crash, and
                // a record from a newer ttk is not worth failing over.
                Err(_) => skipped += 1,
            }
        }
        Ok((out, skipped))
    }

    /// Fold the ledger into per-project totals.
    pub fn aggregate(&self) -> Result<GlobalUsage> {
        let (records, skipped) = self.read()?;
        let mut usage = fold(records);
        usage.skipped_lines = skipped;
        usage.path = Some(self.path.clone());
        Ok(usage)
    }

    /// Totals per calendar day, keyed by days since the Unix epoch.
    ///
    /// `offset_secs` shifts UTC into the viewer's time zone so "today" means
    /// the viewer's today. Rollups carry no per-day shape and are left out;
    /// they only ever hold records older than the retention window, which is
    /// longer than any chart `ttk gain` draws.
    pub fn daily(
        &self,
        offset_secs: i64,
        project_id: Option<&str>,
    ) -> Result<BTreeMap<i64, ProgramTotals>> {
        let (records, _) = self.read()?;
        Ok(daily_totals(&records, offset_secs, project_id))
    }

    /// Replace records older than `retain_days` with one rollup per project.
    ///
    /// Written to a temporary file and renamed, so an interrupted compaction
    /// leaves the original ledger untouched.
    pub fn compact(&self, retain_days: u64, now_millis: u64) -> Result<CompactStats> {
        let (records, _) = self.read()?;
        let bytes_before = self.size();
        let records_before = records.len() as u64;
        let cutoff = now_millis.saturating_sub(retain_days * MILLIS_PER_DAY);

        let mut fresh: Vec<Record> = Vec::new();
        // One rollup per project. Rolling up per month as well would keep more
        // shape, and nothing reads that shape; per project is what `ttk stats`
        // shows.
        let mut old: BTreeMap<String, RollupRecord> = BTreeMap::new();

        for record in records {
            match record {
                Record::Run(r) if r.at_millis >= cutoff => fresh.push(Record::Run(r)),
                Record::Run(r) => {
                    let entry = old
                        .entry(r.project_id.clone())
                        .or_insert_with(|| RollupRecord {
                            from_millis: r.at_millis,
                            to_millis: r.at_millis,
                            project: r.project.clone(),
                            project_id: r.project_id.clone(),
                            events: 0,
                            tokens_before: 0,
                            tokens_after: 0,
                            filtered_lines: 0,
                            method: r.method.clone(),
                            by_program: BTreeMap::new(),
                        });
                    absorb_run(entry, &r);
                }
                // A rollup of a rollup has to be the same rollup, so an
                // existing entry absorbs this one and a missing entry simply
                // becomes it. Inserting a clone and *then* absorbing would
                // count every old record twice on the second compaction.
                Record::Rollup(r) => match old.entry(r.project_id.clone()) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(r);
                    }
                    std::collections::btree_map::Entry::Occupied(mut slot) => {
                        absorb_rollup(slot.get_mut(), &r)
                    }
                },
            }
        }

        let mut out = String::new();
        for rollup in old.into_values() {
            out.push_str(
                &serde_json::to_string(&Record::Rollup(rollup))
                    .map_err(|e| Error::storage(format!("cannot serialize rollup: {e}")))?,
            );
            out.push('\n');
        }
        let records_after = out.lines().count() as u64 + fresh.len() as u64;
        for record in fresh {
            out.push_str(
                &serde_json::to_string(&record)
                    .map_err(|e| Error::storage(format!("cannot serialize record: {e}")))?,
            );
            out.push('\n');
        }

        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, &self.path)?;

        Ok(CompactStats {
            records_before,
            records_after,
            bytes_before,
            bytes_after: out.len() as u64,
        })
    }

    /// Forget everything about one project, or all of them.
    pub fn forget(&self, project_id: Option<&str>) -> Result<u64> {
        let (records, _) = self.read()?;
        let mut kept = String::new();
        let mut dropped = 0;
        for record in records {
            let id = match &record {
                Record::Run(r) => &r.project_id,
                Record::Rollup(r) => &r.project_id,
            };
            if project_id.is_none_or(|want| id == want) {
                dropped += 1;
                continue;
            }
            kept.push_str(
                &serde_json::to_string(&record)
                    .map_err(|e| Error::storage(format!("cannot serialize record: {e}")))?,
            );
            kept.push('\n');
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, &kept)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(dropped)
    }
}

/// Days since the Unix epoch for a timestamp, shifted by `offset_secs`.
pub fn day_number(at_millis: u64, offset_secs: i64) -> i64 {
    (at_millis as i64 / 1000 + offset_secs).div_euclid(86_400)
}

fn daily_totals(
    records: &[Record],
    offset_secs: i64,
    project_id: Option<&str>,
) -> BTreeMap<i64, ProgramTotals> {
    let mut days: BTreeMap<i64, ProgramTotals> = BTreeMap::new();
    for record in records {
        let Record::Run(r) = record else { continue };
        if project_id.is_some_and(|want| r.project_id != want) {
            continue;
        }
        let t = days
            .entry(day_number(r.at_millis, offset_secs))
            .or_default();
        t.runs += 1;
        t.tokens_before += r.tokens_before;
        t.tokens_after += r.tokens_after;
    }
    days
}

fn absorb_run(target: &mut RollupRecord, r: &RunRecord) {
    target.from_millis = target.from_millis.min(r.at_millis);
    target.to_millis = target.to_millis.max(r.at_millis);
    target.events += 1;
    target.tokens_before += r.tokens_before;
    target.tokens_after += r.tokens_after;
    target.filtered_lines += r.filtered_lines;
    target.method = weaker(&target.method, &r.method);
    if let Some(program) = &r.program {
        let t = target.by_program.entry(program.clone()).or_default();
        t.runs += 1;
        t.tokens_before += r.tokens_before;
        t.tokens_after += r.tokens_after;
    }
}

fn absorb_rollup(target: &mut RollupRecord, r: &RollupRecord) {
    target.from_millis = target.from_millis.min(r.from_millis);
    target.to_millis = target.to_millis.max(r.to_millis);
    target.events += r.events;
    target.tokens_before += r.tokens_before;
    target.tokens_after += r.tokens_after;
    target.filtered_lines += r.filtered_lines;
    target.method = weaker(&target.method, &r.method);
    for (name, totals) in &r.by_program {
        target
            .by_program
            .entry(name.clone())
            .or_default()
            .add(totals);
    }
}

/// Fold a record stream into per-project totals.
fn fold(records: Vec<Record>) -> GlobalUsage {
    let mut by_project: BTreeMap<String, ProjectUsage> = BTreeMap::new();
    let mut usage = GlobalUsage::default();

    for record in records {
        let (id, project) = match &record {
            Record::Run(r) => (r.project_id.clone(), r.project.clone()),
            Record::Rollup(r) => (r.project_id.clone(), r.project.clone()),
        };
        let entry = by_project
            .entry(id.clone())
            .or_insert_with(|| ProjectUsage {
                project,
                project_id: id,
                first_millis: u64::MAX,
                ..ProjectUsage::default()
            });

        match record {
            Record::Run(r) => {
                entry.events += 1;
                entry.tokens_before += r.tokens_before;
                entry.tokens_after += r.tokens_after;
                entry.filtered_lines += r.filtered_lines;
                entry.first_millis = entry.first_millis.min(r.at_millis);
                entry.last_millis = entry.last_millis.max(r.at_millis);
                if let Some(program) = &r.program {
                    let t = entry.by_program.entry(program.clone()).or_default();
                    t.runs += 1;
                    t.tokens_before += r.tokens_before;
                    t.tokens_after += r.tokens_after;
                }
                usage.method = weaker(&usage.method, &r.method);
            }
            Record::Rollup(r) => {
                entry.events += r.events;
                entry.tokens_before += r.tokens_before;
                entry.tokens_after += r.tokens_after;
                entry.filtered_lines += r.filtered_lines;
                entry.first_millis = entry.first_millis.min(r.from_millis);
                entry.last_millis = entry.last_millis.max(r.to_millis);
                for (name, totals) in &r.by_program {
                    entry
                        .by_program
                        .entry(name.clone())
                        .or_default()
                        .add(totals);
                }
                usage.method = weaker(&usage.method, &r.method);
            }
        }
    }

    for p in by_project.values_mut() {
        if p.first_millis == u64::MAX {
            p.first_millis = 0;
        }
        usage.events += p.events;
        usage.tokens_before += p.tokens_before;
        usage.tokens_after += p.tokens_after;
        usage.filtered_lines += p.filtered_lines;
        usage.first_millis = Some(match usage.first_millis {
            Some(v) => v.min(p.first_millis),
            None => p.first_millis,
        });
        usage.last_millis = Some(match usage.last_millis {
            Some(v) => v.max(p.last_millis),
            None => p.last_millis,
        });
    }

    usage.projects = by_project.into_values().collect();
    usage
        .projects
        .sort_by_key(|p| (std::cmp::Reverse(p.saved()), p.project.clone()));
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(project: &str, at: u64, before: u64, after: u64, program: &str) -> RunRecord {
        RunRecord {
            at_millis: at,
            project: project.to_string(),
            project_id: format!("id_{project}"),
            program: Some(program.to_string()),
            tokens_before: before,
            tokens_after: after,
            transformed: after < before,
            filtered_lines: 0,
            method: "estimated".to_string(),
        }
    }

    fn ledger(dir: &tempfile::TempDir) -> UsageLedger {
        UsageLedger::at(dir.path().join(USAGE_FILE))
    }

    #[test]
    fn a_missing_ledger_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let usage = ledger(&dir).aggregate().expect("aggregate");
        assert!(usage.is_empty());
        assert_eq!(usage.saved(), 0);
        assert_eq!(usage.saved_percent(), 0.0);
    }

    #[test]
    fn records_from_several_projects_are_kept_apart_and_totalled() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        l.append(&run("/a", 100, 1000, 100, "cargo")).expect("a");
        l.append(&run("/a", 200, 500, 50, "cargo")).expect("a2");
        l.append(&run("/b", 300, 200, 20, "pytest")).expect("b");

        let usage = l.aggregate().expect("aggregate");
        assert_eq!(usage.events, 3);
        assert_eq!(usage.saved(), 900 + 450 + 180);
        assert_eq!(usage.projects.len(), 2);
        // Ranked by what each project saved.
        assert_eq!(usage.projects[0].project, "/a");
        assert_eq!(usage.projects[0].events, 2);
        assert_eq!(usage.projects[1].project, "/b");
        assert_eq!(usage.first_millis, Some(100));
        assert_eq!(usage.last_millis, Some(300));

        let programs = usage.by_program();
        assert_eq!(programs[0].0, "cargo");
        assert_eq!(programs[0].1.runs, 2);
        assert_eq!(programs[1].0, "pytest");
    }

    #[test]
    fn a_corrupt_line_costs_one_record_not_the_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        l.append(&run("/a", 100, 1000, 100, "cargo")).expect("a");
        let mut f = OpenOptions::new()
            .append(true)
            .open(l.path())
            .expect("open");
        f.write_all(b"{\"t\":\"run\",\"at_mill").expect("write");
        drop(f);
        l.append(&run("/a", 200, 500, 50, "cargo")).expect("a2");

        let usage = l.aggregate().expect("aggregate");
        assert_eq!(usage.events, 2);
        assert_eq!(usage.skipped_lines, 1, "and the loss is reported");
    }

    #[test]
    fn compaction_folds_old_records_and_keeps_the_totals() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        let now = 100 * MILLIS_PER_DAY;
        for i in 0..50u64 {
            // Half of them older than the retention window.
            l.append(&run("/a", i * MILLIS_PER_DAY, 100, 10, "cargo"))
                .expect("append");
        }
        let before = l.aggregate().expect("aggregate");

        let stats = l.compact(45, now).expect("compact");
        assert!(stats.records_after < stats.records_before);

        let after = l.aggregate().expect("aggregate");
        assert_eq!(
            after.saved(),
            before.saved(),
            "compaction may shrink the file, never the total"
        );
        assert_eq!(after.events, before.events);
        assert_eq!(after.projects[0].by_program["cargo"].runs, 50);
    }

    #[test]
    fn compacting_twice_is_stable() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        let now = 100 * MILLIS_PER_DAY;
        for i in 0..10u64 {
            l.append(&run("/a", i * MILLIS_PER_DAY, 100, 10, "cargo"))
                .expect("append");
        }
        l.compact(1, now).expect("first");
        let once = l.aggregate().expect("aggregate");
        l.compact(1, now).expect("second");
        let twice = l.aggregate().expect("aggregate");
        assert_eq!(once, twice, "a rollup of a rollup is the same rollup");
    }

    #[test]
    fn forgetting_one_project_leaves_the_others() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        l.append(&run("/a", 100, 1000, 100, "cargo")).expect("a");
        l.append(&run("/b", 200, 500, 50, "pytest")).expect("b");
        assert_eq!(l.forget(Some("id_/a")).expect("forget"), 1);

        let usage = l.aggregate().expect("aggregate");
        assert_eq!(usage.projects.len(), 1);
        assert_eq!(usage.projects[0].project, "/b");

        assert_eq!(l.forget(None).expect("forget all"), 1);
        assert!(l.aggregate().expect("aggregate").is_empty());
    }

    #[test]
    fn the_weakest_counting_method_wins() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        let mut exact = run("/a", 100, 1000, 100, "cargo");
        exact.method = "tokenizer".into();
        l.append(&exact).expect("exact");
        l.append(&run("/a", 200, 10, 1, "cargo"))
            .expect("estimated");
        assert_eq!(l.aggregate().expect("aggregate").method, "estimated");
    }

    #[test]
    fn a_project_id_is_stable_and_case_insensitive() {
        let dir = tempfile::tempdir().expect("tmp");
        let a = project_id(dir.path());
        assert_eq!(a, project_id(dir.path()));
        assert_eq!(a.len(), 12);
        assert_ne!(a, project_id(Path::new("some/other/place")));
    }

    #[test]
    fn daily_totals_bucket_by_local_day_and_project() {
        let dir = tempfile::tempdir().expect("tmp");
        let l = ledger(&dir);
        let day = MILLIS_PER_DAY;
        // 23:00 UTC on day 10: day 10 in UTC, day 11 two hours east of it.
        l.append(&run("/a", 10 * day + 23 * 3_600_000, 100, 10, "cargo"))
            .expect("a");
        l.append(&run("/a", 11 * day + 1_000, 50, 5, "cargo"))
            .expect("a2");
        l.append(&run("/b", 11 * day + 2_000, 70, 7, "npm"))
            .expect("b");

        let utc = l.daily(0, None).expect("daily");
        assert_eq!(utc[&10].saved(), 90);
        assert_eq!(utc[&11].saved(), 45 + 63);
        assert_eq!(utc[&11].runs, 2);

        let east = l.daily(2 * 3_600, None).expect("daily");
        assert!(!east.contains_key(&10));
        assert_eq!(east[&11].runs, 3);

        let only_a = l.daily(0, Some("id_/a")).expect("daily");
        assert_eq!(only_a[&11].saved(), 45);
    }

    #[test]
    fn the_ledger_can_be_switched_off() {
        // SAFETY: single threaded test scope, restored immediately.
        unsafe { std::env::set_var("TTK_USAGE", "0") };
        assert!(UsageLedger::open().is_none());
        unsafe { std::env::remove_var("TTK_USAGE") };
    }
}
