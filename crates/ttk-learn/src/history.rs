//! Versioned rule files: every overwrite can be undone.
//!
//! A rule file is accumulated knowledge, and the commands that rewrite it —
//! `learn`, `forget`, `prune`, `import` — are exactly the ones an agent runs
//! without much ceremony. So before a rule file is replaced, the bytes it held
//! are copied aside:
//!
//! ```text
//! <dir>/learned-filters.json
//! <dir>/rules-history/learned-filters/1764000000000-1a2b3c4d.json
//! <dir>/rules-history/learned-filters/index.jsonl   {id, at_millis, reason, rules}
//! ```
//!
//! The history lives in a sub-directory, so the global filters folder, which
//! loads every `*.json` file directly inside it, never mistakes a snapshot for
//! a live rule file.
//!
//! # Concurrency
//!
//! Two `ttk` processes doing load → modify → save on the same file lose one of
//! the two updates unless the whole sequence is serialised. [`with_lock`] does
//! that with an OS file lock on `<file>.lock`. The functions here do **not**
//! take the lock themselves — a second lock on the same file from inside a
//! locked section would wait on itself — so callers wrap the whole
//! read-modify-write in [`with_lock`].

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ttk_core::{Error, Result};

use crate::ruleset::RuleSet;

/// Directory, next to the rule file, that holds the snapshots.
pub const HISTORY_DIR: &str = "rules-history";

/// Snapshots kept per rule file. Older ones are deleted.
pub const MAX_VERSIONS: usize = 50;

const INDEX_FILE: &str = "index.jsonl";

/// One snapshot of a rule file, as it was just before it was overwritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// `<millis>-<8 hex of the content hash>`.
    pub id: String,
    pub at_millis: u64,
    /// Why the file was overwritten (`learn`, `prune`, `restore`, …).
    pub reason: String,
    /// Rules in the snapshot; `0` if it was not a valid rule file.
    pub rules: usize,
    /// Where the snapshot's bytes are.
    pub path: PathBuf,
}

/// One line of `index.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    at_millis: u64,
    reason: String,
    rules: usize,
}

/// `<parent>/rules-history/<stem>/`.
pub fn history_dir(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rules".to_string());
    parent.join(HISTORY_DIR).join(stem)
}

/// `<path>.lock`: the full file name plus `.lock`, so `learned-filters.json`
/// locks on `learned-filters.json.lock`.
pub fn lock_path(path: &Path) -> PathBuf {
    with_suffix(path, ".lock")
}

/// Run `f` while holding an exclusive OS lock on `<path>.lock`.
///
/// Blocks until the lock is free. The lock is released when `f` returns or
/// panics; the lock file itself is left in place, because deleting it would
/// let a third process lock a fresh file while a second still holds the old.
pub fn with_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = lock_path(path);
    if let Some(parent) = lock.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .map_err(|e| Error::storage(format!("cannot open {}: {e}", lock.display())))?;
    file.lock()
        .map_err(|e| Error::storage(format!("cannot lock {}: {e}", lock.display())))?;
    let out = f();
    // Dropping the handle releases the lock as well; unlocking explicitly just
    // makes the release independent of when the handle happens to drop.
    let _ = file.unlock();
    out
}

/// Save `set` to `path`, first snapshotting what the file held if that
/// differs from what is about to be written.
///
/// Returns the snapshot that was made, or `None` when there was nothing to
/// keep: the file did not exist, or already held exactly these bytes (in
/// which case it is not rewritten either).
pub fn save_versioned(set: &RuleSet, path: &Path, reason: &str) -> Result<Option<Version>> {
    write_versioned(path, &set.file_text(), reason)
}

/// Every snapshot of `path`, newest first.
pub fn versions(path: &Path) -> Result<Vec<Version>> {
    let dir = history_dir(path);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = read_index(&dir)?;
    // A snapshot whose index line was lost (a crash between the two writes,
    // or a hand-edited index) is still a snapshot.
    let mut orphans: Vec<Entry> = Vec::new();
    for item in fs::read_dir(&dir)? {
        let p = item?.path();
        let Some(id) = snapshot_id(&p) else {
            continue;
        };
        if entries.iter().any(|e| e.id == id) {
            continue;
        }
        let at_millis = id
            .split('-')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        orphans.push(Entry {
            rules: fs::read(&p).map(|b| count_rules(&b)).unwrap_or(0),
            id,
            at_millis,
            reason: "(unindexed)".to_string(),
        });
    }
    orphans.sort_by(|a, b| a.id.cmp(&b.id));
    entries.extend(orphans);

    // Index order is write order; reversing it and then sorting stably by
    // time makes two snapshots taken in the same millisecond come out in the
    // order they were really taken.
    entries.retain(|e| dir.join(format!("{}.json", e.id)).is_file());
    entries.reverse();
    entries.sort_by_key(|e| std::cmp::Reverse(e.at_millis));
    Ok(entries
        .into_iter()
        .map(|e| Version {
            path: dir.join(format!("{}.json", e.id)),
            id: e.id,
            at_millis: e.at_millis,
            reason: e.reason,
            rules: e.rules,
        })
        .collect())
}

/// Put the snapshot `version_id_or_prefix` back as the live file.
///
/// The current file is snapshotted first, with reason `restore`, so a
/// restore can itself be undone. The snapshot must parse as a rule file; one
/// that does not is refused and the live file is left untouched.
pub fn restore(path: &Path, version_id_or_prefix: &str) -> Result<Version> {
    let needle = version_id_or_prefix.trim();
    if needle.is_empty() {
        return Err(Error::other("no version given"));
    }
    let all = versions(path)?;
    let version = match all.iter().find(|v| v.id == needle) {
        Some(v) => v.clone(),
        None => {
            let mut hits = all.iter().filter(|v| v.id.starts_with(needle));
            match (hits.next(), hits.next()) {
                (Some(v), None) => v.clone(),
                (Some(_), Some(_)) => {
                    return Err(Error::other(format!(
                        "`{needle}` matches more than one version — use the full id"
                    )));
                }
                _ => {
                    return Err(Error::other(format!(
                        "no version of {} matching `{needle}`",
                        path.display()
                    )));
                }
            }
        }
    };
    let bytes = fs::read(&version.path)
        .map_err(|e| Error::storage(format!("cannot read {}: {e}", version.path.display())))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::other(format!(
            "version {} is not text and cannot be restored",
            version.id
        ))
    })?;
    RuleSet::from_json(&text).map_err(|e| {
        Error::other(format!(
            "version {} is not a valid rule file and cannot be restored: {e}",
            version.id
        ))
    })?;
    write_versioned(path, &text, "restore")?;
    Ok(version)
}

// ---------------------------------------------------------------------------

fn write_versioned(path: &Path, new_text: &str, reason: &str) -> Result<Option<Version>> {
    let snapshot = match fs::read(path) {
        Ok(old) if old == new_text.as_bytes() => return Ok(None),
        Ok(old) => Some(snapshot(path, &old, reason)?),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Error::storage(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    write_atomic(path, new_text.as_bytes())?;
    Ok(snapshot)
}

/// Copy `old` aside, record it in the index and enforce [`MAX_VERSIONS`].
fn snapshot(path: &Path, old: &[u8], reason: &str) -> Result<Version> {
    let dir = history_dir(path);
    fs::create_dir_all(&dir)?;
    let at_millis = ttk_core::ids::now_millis();
    let hash = blake3::hash(old).to_hex();
    let id = format!("{at_millis}-{}", &hash[..8]);
    let file = dir.join(format!("{id}.json"));
    let rules = count_rules(old);
    write_atomic(&file, old)?;

    let mut entries = read_index(&dir)?;
    // The same bytes snapshotted twice in one millisecond are one snapshot.
    if !entries.iter().any(|e| e.id == id) {
        let entry = Entry {
            id: id.clone(),
            at_millis,
            reason: reason.to_string(),
            rules,
        };
        let mut line = serde_json::to_string(&entry).map_err(Error::other)?;
        line.push('\n');
        let mut index = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(INDEX_FILE))?;
        index.write_all(line.as_bytes())?;
        entries.push(entry);
    }
    if entries.len() > MAX_VERSIONS || has_orphans(&dir, &entries)? {
        evict(path, &dir)?;
    }
    Ok(Version {
        id,
        at_millis,
        reason: reason.to_string(),
        rules,
        path: file,
    })
}

/// Keep the newest [`MAX_VERSIONS`], delete the rest, rewrite the index.
fn evict(path: &Path, dir: &Path) -> Result<()> {
    let all = versions(path)?;
    if all.len() <= MAX_VERSIONS {
        return Ok(());
    }
    let (keep, drop) = all.split_at(MAX_VERSIONS);
    for v in drop {
        let _ = fs::remove_file(&v.path);
    }
    // Back to write order (oldest first) for the index.
    let mut lines = String::new();
    for v in keep.iter().rev() {
        let entry = Entry {
            id: v.id.clone(),
            at_millis: v.at_millis,
            reason: v.reason.clone(),
            rules: v.rules,
        };
        lines.push_str(&serde_json::to_string(&entry).map_err(Error::other)?);
        lines.push('\n');
    }
    write_atomic(&dir.join(INDEX_FILE), lines.as_bytes())
}

fn has_orphans(dir: &Path, entries: &[Entry]) -> Result<bool> {
    let snapshots = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| snapshot_id(&e.path()).is_some())
        .count();
    Ok(snapshots > entries.len())
}

fn read_index(dir: &Path) -> Result<Vec<Entry>> {
    let text = match fs::read_to_string(dir.join(INDEX_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    // A torn or hand-mangled line costs that line, not the whole history.
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect())
}

/// The id of a snapshot file, or `None` for anything else in the directory.
fn snapshot_id(p: &Path) -> Option<String> {
    if !p.is_file() || p.extension().is_none_or(|e| e != "json") {
        return None;
    }
    let stem = p.file_stem()?.to_str()?;
    let (millis, hash) = stem.split_once('-')?;
    let ok = !millis.is_empty()
        && millis.bytes().all(|b| b.is_ascii_digit())
        && hash.len() == 8
        && hash.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| stem.to_string())
}

fn count_rules(bytes: &[u8]) -> usize {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| RuleSet::from_json(t).ok())
        .map(|s| s.len())
        .unwrap_or(0)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Write via a temporary file and a rename, so a crash never leaves half a
/// file behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp = with_suffix(path, ".tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::annotate;
    use crate::rule::Scope;
    use crate::ruleset::Lesson;

    fn set_with(lines: &[&str]) -> RuleSet {
        let mut text = String::from("<filter-trash>\n");
        for l in lines {
            text.push_str(l);
            text.push('\n');
        }
        text.push_str("</filter-trash>\n");
        let a = annotate::parse(&text);
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::global()));
        set
    }

    fn one() -> RuleSet {
        set_with(&["npm WARN deprecated pkg@1.0.6 unsupported"])
    }

    fn two() -> RuleSet {
        set_with(&[
            "npm WARN deprecated pkg@1.0.6 unsupported",
            "yarn info fetching package metadata now",
        ])
    }

    #[test]
    fn the_first_save_has_nothing_to_snapshot() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        assert_eq!(save_versioned(&one(), &path, "learn").expect("save"), None);
        assert_eq!(RuleSet::load(&path).expect("load").len(), 1);
        assert!(versions(&path).expect("versions").is_empty());
    }

    #[test]
    fn overwriting_keeps_the_previous_content() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        save_versioned(&one(), &path, "learn").expect("save");
        let old = fs::read(&path).expect("read");

        let v = save_versioned(&two(), &path, "learn")
            .expect("save")
            .expect("a snapshot");
        assert_eq!(v.reason, "learn");
        assert_eq!(v.rules, 1, "the snapshot is the *old* file");
        assert_eq!(fs::read(&v.path).expect("snapshot"), old, "byte exact");
        assert!(
            v.path
                .starts_with(dir.path().join(HISTORY_DIR).join("learned-filters"))
        );
        let (millis, hash) = v.id.split_once('-').expect("id shape");
        assert_eq!(millis.parse::<u64>().expect("millis"), v.at_millis);
        assert_eq!(hash.len(), 8);

        let listed = versions(&path).expect("versions");
        assert_eq!(listed, vec![v]);
        assert_eq!(RuleSet::load(&path).expect("load").len(), 2);
    }

    #[test]
    fn saving_identical_content_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        let set = one();
        save_versioned(&set, &path, "learn").expect("save");
        let before = fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(save_versioned(&set, &path, "learn").expect("save"), None);
        let after = fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(before, after, "not rewritten");
        assert!(versions(&path).expect("versions").is_empty());
    }

    #[test]
    fn restore_brings_back_a_version_and_can_itself_be_undone() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        save_versioned(&one(), &path, "learn").expect("save");
        let v1 = save_versioned(&two(), &path, "learn")
            .expect("save")
            .expect("snapshot");
        let two_bytes = fs::read(&path).expect("read");

        let restored = restore(&path, &v1.id[..10]).expect("restore by prefix");
        assert_eq!(restored.id, v1.id);
        assert_eq!(RuleSet::load(&path).expect("load").len(), 1);

        let listed = versions(&path).expect("versions");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].reason, "restore", "newest first");
        assert_eq!(listed[0].rules, 2);
        assert_eq!(fs::read(&listed[0].path).expect("read"), two_bytes);

        // Undo the restore.
        restore(&path, &listed[0].id).expect("restore");
        assert_eq!(RuleSet::load(&path).expect("load").len(), 2);
    }

    #[test]
    fn restore_refuses_unknown_and_ambiguous_ids() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        assert!(restore(&path, "123").is_err(), "no history at all");
        save_versioned(&one(), &path, "learn").expect("save");
        save_versioned(&two(), &path, "learn").expect("save");
        save_versioned(&one(), &path, "learn").expect("save");
        assert_eq!(versions(&path).expect("versions").len(), 2);
        assert!(restore(&path, "nope").is_err());
        assert!(restore(&path, "").is_err());
        // Every id starts with the same millisecond digits.
        let err = restore(&path, "1").expect_err("ambiguous");
        assert!(err.to_string().contains("more than one"), "{err}");
    }

    #[test]
    fn a_broken_snapshot_is_never_restored() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        fs::write(&path, "{ this is not json").expect("write");
        let v = save_versioned(&one(), &path, "learn")
            .expect("save")
            .expect("even a broken file is kept");
        assert_eq!(v.rules, 0);
        let before = fs::read(&path).expect("read");
        assert!(restore(&path, &v.id).is_err());
        assert_eq!(
            fs::read(&path).expect("read"),
            before,
            "live file untouched"
        );
    }

    #[test]
    fn only_the_newest_versions_are_kept() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        let base = one();
        let total = MAX_VERSIONS + 5;
        for i in 0..total {
            let mut set = base.clone();
            set.rules[0].note = Some(format!("v{i}"));
            save_versioned(&set, &path, &format!("save {i}")).expect("save");
        }
        let listed = versions(&path).expect("versions");
        assert_eq!(listed.len(), MAX_VERSIONS);
        // Save i snapshots the content of save i - 1.
        let newest = fs::read_to_string(&listed[0].path).expect("read");
        assert!(newest.contains(&format!("\"v{}\"", total - 2)), "{newest}");
        assert_eq!(listed[0].reason, format!("save {}", total - 1));
        let oldest = fs::read_to_string(&listed[MAX_VERSIONS - 1].path).expect("read");
        let first_kept = total - 1 - MAX_VERSIONS;
        assert!(oldest.contains(&format!("\"v{first_kept}\"")), "{oldest}");

        let snapshots = fs::read_dir(history_dir(&path))
            .expect("dir")
            .filter(|e| snapshot_id(&e.as_ref().expect("entry").path()).is_some())
            .count();
        assert_eq!(snapshots, MAX_VERSIONS, "evicted files are deleted");
    }

    #[test]
    fn a_snapshot_without_an_index_line_is_still_listed() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        save_versioned(&one(), &path, "learn").expect("save");
        save_versioned(&two(), &path, "learn").expect("save");
        fs::remove_file(history_dir(&path).join(INDEX_FILE)).expect("rm index");
        let listed = versions(&path).expect("versions");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].reason, "(unindexed)");
        assert_eq!(listed[0].rules, 1);
    }

    #[test]
    fn history_is_not_loaded_as_a_rule_file() {
        // The global folder loads `*.json` files directly inside it; the
        // snapshots sit one level further down.
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("learned-filters.json");
        let h = history_dir(&path);
        assert_eq!(h.parent().expect("parent").parent(), Some(dir.path()));
        assert_eq!(
            lock_path(&path).file_name().and_then(|n| n.to_str()),
            Some("learned-filters.json.lock")
        );
    }

    #[test]
    fn the_lock_serialises_read_modify_write() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("counter.json");
        fs::write(&path, "0").expect("write");
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        with_lock(&path, || {
                            let n: u64 = fs::read_to_string(&path)?.trim().parse().unwrap_or(0);
                            // Widen the race window: without the lock, another
                            // thread reads the same `n` here.
                            std::thread::yield_now();
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            fs::write(&path, (n + 1).to_string())?;
                            Ok(())
                        })
                        .expect("locked update");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread");
        }
        assert_eq!(fs::read_to_string(&path).expect("read"), "80");
        assert!(lock_path(&path).is_file(), "the lock file stays");
    }

    #[test]
    fn the_lock_passes_the_closures_result_through() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("x.json");
        assert_eq!(with_lock(&path, || Ok(7)).expect("ok"), 7);
        let err = with_lock::<()>(&path, || Err(Error::other("boom"))).expect_err("err");
        assert!(err.to_string().contains("boom"));
        // Released after an error: taking it again does not hang.
        with_lock(&path, || Ok(())).expect("relock");
    }
}
