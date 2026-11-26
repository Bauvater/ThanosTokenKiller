//! ThanosTokenKiller storage layer.
//!
//! Everything lives in a single directory, by default `<project>/.ttk`:
//!
//! ```text
//! .ttk/
//!   config.toml     project configuration (optional)
//!   blobs/          content addressed, zstd compressed originals
//!   index.redb      capsule metadata
//!   events/         one JSONL file per session
//!   learned-filters.json  rules an agent taught, meant to be committed
//! ```
//!
//! One thing deliberately lives *outside* every project: the global usage
//! ledger in [`usage`]. "How much has ttk saved me, everywhere" is the number
//! people actually want, and answering it by walking twenty `.ttk` directories
//! is how a statistic becomes useless.
//!
//! No project's data is ever mixed with another's: the workspace is resolved
//! from the project root and every path is derived from it.

pub mod blob;
pub mod capsule;
pub mod events;
pub mod provider;
pub mod usage;

use std::path::{Path, PathBuf};

use ttk_core::Result;
use ttk_core::config::Config;

pub use blob::{BlobRef, BlobStore, Compression};
pub use capsule::{Capsule, CapsuleStore, GcStats, LEVEL_RAW, NewCapsule, StoreStats};
pub use events::EventLog;
pub use provider::{MeasuredUsage, ProviderStore};
pub use usage::{GlobalUsage, ProjectUsage, RecordKind, RunRecord, SessionUsage, UsageLedger};

/// Directory name used inside a project.
pub const WORKSPACE_DIR: &str = ".ttk";

/// An opened storage workspace.
pub struct Workspace {
    root: PathBuf,
    capsules: CapsuleStore,
    events: EventLog,
}

impl Workspace {
    /// Resolve the workspace directory for `cwd` without opening it.
    ///
    /// `TTK_HOME` overrides the location entirely, which is what the test
    /// suite and CI use to stay off the developer's real store.
    pub fn locate(cwd: &Path) -> PathBuf {
        if let Ok(home) = std::env::var("TTK_HOME")
            && !home.trim().is_empty()
        {
            return PathBuf::from(home);
        }
        match ttk_core::config::find_project_root(cwd) {
            Some(root) => root.join(WORKSPACE_DIR),
            None => cwd.join(WORKSPACE_DIR),
        }
    }

    pub fn open(cwd: &Path, config: &Config) -> Result<Self> {
        Self::open_at(&Self::locate(cwd), config)
    }

    pub fn open_at(root: &Path, config: &Config) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let compression = Compression::from_config(
            &config.capsules.compression,
            config.capsules.compression_level,
        );
        let blobs = BlobStore::open(root.join("blobs"), compression)?;
        let capsules = CapsuleStore::open(
            &root.join("index.redb"),
            blobs,
            config.capsules.max_storage_gb,
        )?;
        let events = EventLog::open(root.join("events"), config.telemetry.enabled)?;
        Ok(Self {
            root: root.to_path_buf(),
            capsules,
            events,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn capsules(&self) -> &CapsuleStore {
        &self.capsules
    }

    pub fn events(&self) -> &EventLog {
        &self.events
    }

    /// Collect garbage if the last collection is older than
    /// `capsules.auto_gc_hours`. Cheap when it is not due: one `metadata`
    /// call on a stamp file.
    ///
    /// Only the process holding the workspace can run this (the capsule index
    /// admits one process at a time), and the stamp is written before the
    /// work starts, so parallel agents never collect twice in a row.
    pub fn maybe_gc(&self, config: &Config, now_millis: u64) -> Result<Option<AutoGc>> {
        let hours = config.capsules.auto_gc_hours;
        if hours == 0 {
            return Ok(None);
        }
        let stamp = self.root.join(GC_STAMP);
        let due = match std::fs::metadata(&stamp).and_then(|m| m.modified()) {
            Ok(at) => at
                .elapsed()
                .map(|age| age.as_secs() >= u64::from(hours) * 3600)
                .unwrap_or(true),
            Err(_) => true,
        };
        if !due {
            return Ok(None);
        }
        std::fs::write(&stamp, now_millis.to_string())?;
        let capsules = self.capsules.gc(now_millis)?;
        let events_removed = self.prune_events(config.capsules.events_ttl_days)?;
        Ok(Some(AutoGc {
            capsules,
            events_removed,
        }))
    }

    /// Delete per-session event logs not written to for `days` days.
    fn prune_events(&self, days: u32) -> Result<u64> {
        if days == 0 {
            return Ok(0);
        }
        let max_age = std::time::Duration::from_secs(u64::from(days) * 86_400);
        let Ok(entries) = std::fs::read_dir(self.events.root()) else {
            return Ok(0);
        };
        let mut removed = 0;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if old && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Stamp file recording the last automatic collection.
pub const GC_STAMP: &str = "gc.stamp";

/// What one automatic collection did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoGc {
    pub capsules: GcStats,
    pub events_removed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttk_core::content::ContentType;

    #[test]
    fn workspace_wires_everything_together() {
        let dir = tempfile::tempdir().expect("tmp");
        let cfg = Config::default();
        let ws = Workspace::open_at(&dir.path().join(".ttk"), &cfg).expect("open");
        assert!(ws.root().join("blobs").is_dir());
        assert!(ws.root().join("events").is_dir());

        let c = ws
            .capsules()
            .put(NewCapsule::new("payload", ContentType::PlainText))
            .expect("put");
        assert_eq!(
            ws.capsules()
                .render(c.id.as_str(), LEVEL_RAW, false)
                .expect("render"),
            "payload"
        );
    }

    #[test]
    fn locate_prefers_ttk_home() {
        let dir = tempfile::tempdir().expect("tmp");
        // SAFETY: single-threaded test scope; restored immediately after.
        unsafe { std::env::set_var("TTK_HOME", dir.path()) };
        assert_eq!(Workspace::locate(Path::new(".")), dir.path());
        unsafe { std::env::remove_var("TTK_HOME") };
    }

    fn age(path: &Path, days: u64) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open");
        f.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400),
        )
        .expect("set mtime");
    }

    #[test]
    fn automatic_gc_runs_when_due_and_only_then() {
        let dir = tempfile::tempdir().expect("tmp");
        let config = Config::default();
        let ws = Workspace::open_at(&dir.path().join(".ttk"), &config).expect("ws");
        let now = ttk_core::ids::now_millis();

        // Due on first use, then not again until the interval has passed.
        assert!(ws.maybe_gc(&config, now).expect("gc").is_some());
        assert!(ws.maybe_gc(&config, now).expect("gc").is_none());
        age(&ws.root().join(GC_STAMP), 1);
        assert!(ws.maybe_gc(&config, now).expect("gc").is_some());

        // Switched off: never.
        let mut off = config.clone();
        off.capsules.auto_gc_hours = 0;
        age(&ws.root().join(GC_STAMP), 10);
        assert!(ws.maybe_gc(&off, now).expect("gc").is_none());
    }

    #[test]
    fn automatic_gc_removes_expired_capsules_and_old_event_logs() {
        let dir = tempfile::tempdir().expect("tmp");
        let config = Config::default();
        let ws = Workspace::open_at(&dir.path().join(".ttk"), &config).expect("ws");
        let cap = ws
            .capsules()
            .put(NewCapsule::new("old output\n", ContentType::PlainText).ttl_days(1))
            .expect("put");
        let events = ws.events().root().to_path_buf();
        std::fs::create_dir_all(&events).expect("mkdir");
        let stale = events.join("se_OLD.jsonl");
        let fresh = events.join("se_NEW.jsonl");
        std::fs::write(&stale, "{}\n").expect("w");
        std::fs::write(&fresh, "{}\n").expect("w");
        age(&stale, 400);

        // Two days later the one-day capsule has expired.
        let later = ttk_core::ids::now_millis() + 2 * 86_400_000;
        let done = ws.maybe_gc(&config, later).expect("gc").expect("due");
        assert_eq!(done.capsules.expired_removed, 1);
        assert_eq!(done.events_removed, 1);
        assert!(!stale.exists() && fresh.exists());
        assert!(
            ws.capsules().get(cap.id.as_str()).is_err(),
            "the capsule is gone"
        );
    }
}
