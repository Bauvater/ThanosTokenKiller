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
//! ```
//!
//! No project's data is ever mixed with another's: the workspace is resolved
//! from the project root and every path is derived from it.

pub mod blob;
pub mod capsule;
pub mod events;

use std::path::{Path, PathBuf};

use ttk_core::Result;
use ttk_core::config::Config;

pub use blob::{BlobRef, BlobStore, Compression};
pub use capsule::{Capsule, CapsuleStore, GcStats, LEVEL_RAW, NewCapsule, StoreStats};
pub use events::EventLog;

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
}
