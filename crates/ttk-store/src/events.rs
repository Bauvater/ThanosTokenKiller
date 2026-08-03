//! Local, append only event log (JSON Lines).
//!
//! One file per session under `.ttk/events/<session>.jsonl`. JSONL was chosen
//! so the log stays greppable, exportable and replayable without any tooling,
//! and so a truncated last line (crash during write) costs at most one event.
//!
//! Nothing here ever leaves the machine.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use ttk_core::event::TokenEvent;
use ttk_core::ids::SessionId;
use ttk_core::{Error, Result};

pub struct EventLog {
    root: PathBuf,
    enabled: bool,
}

impl EventLog {
    pub fn open(root: impl Into<PathBuf>, enabled: bool) -> Result<Self> {
        let root = root.into();
        if enabled {
            fs::create_dir_all(&root)?;
        }
        Ok(Self { root, enabled })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn path_for(&self, session: &SessionId) -> PathBuf {
        self.root.join(format!("{session}.jsonl"))
    }

    /// Append one event. A disabled log silently does nothing, which keeps
    /// call sites free of conditionals.
    pub fn append(&self, event: &TokenEvent) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut line = serde_json::to_string(event)
            .map_err(|e| Error::storage(format!("cannot serialize event: {e}")))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path_for(&event.session_id))?;
        // One write syscall per event keeps interleaving safe enough for the
        // single-process case and cheap enough for the hot path.
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Read a session. Corrupt trailing lines are skipped, not fatal.
    pub fn read_session(&self, session: &SessionId) -> Result<Vec<TokenEvent>> {
        let path = self.path_for(session);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let reader = BufReader::new(File::open(&path)?);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TokenEvent>(&line) {
                Ok(e) => out.push(e),
                // A partially written last line is expected after a crash.
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    /// Sessions, newest first (session ids are time sortable).
    pub fn sessions(&self) -> Result<Vec<SessionId>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                out.push(SessionId::from_string(stem));
            }
        }
        out.sort();
        out.reverse();
        Ok(out)
    }

    pub fn latest_session(&self) -> Result<Option<SessionId>> {
        Ok(self.sessions()?.into_iter().next())
    }

    /// Find one event by (prefix of) its id across all sessions.
    pub fn find_event(&self, id_prefix: &str) -> Result<Option<TokenEvent>> {
        for session in self.sessions()? {
            for event in self.read_session(&session)? {
                if event.id.as_str().starts_with(id_prefix) {
                    return Ok(Some(event));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttk_core::content::ContentType;
    use ttk_core::event::{EventDirection, EventSource};

    fn event(session: &SessionId, text: &str) -> TokenEvent {
        TokenEvent::capture(
            session.clone(),
            EventSource::ShellOutput,
            EventDirection::Inbound,
            ContentType::ShellOutput,
            text,
        )
    }

    #[test]
    fn appends_and_reads_back() {
        let dir = tempfile::tempdir().expect("tmp");
        let log = EventLog::open(dir.path(), true).expect("open");
        let s = SessionId::new();
        log.append(&event(&s, "one")).expect("append");
        log.append(&event(&s, "two")).expect("append");
        let events = log.read_session(&s).expect("read");
        assert_eq!(events.len(), 2);
        assert_eq!(log.sessions().expect("sessions"), vec![s]);
    }

    #[test]
    fn disabled_log_writes_nothing() {
        let dir = tempfile::tempdir().expect("tmp");
        let log = EventLog::open(dir.path().join("events"), false).expect("open");
        let s = SessionId::new();
        log.append(&event(&s, "x")).expect("append");
        assert!(log.read_session(&s).expect("read").is_empty());
        assert!(log.sessions().expect("sessions").is_empty());
    }

    #[test]
    fn truncated_last_line_is_survivable() {
        let dir = tempfile::tempdir().expect("tmp");
        let log = EventLog::open(dir.path(), true).expect("open");
        let s = SessionId::new();
        log.append(&event(&s, "good")).expect("append");
        let mut f = OpenOptions::new()
            .append(true)
            .open(log.path_for(&s))
            .expect("open");
        f.write_all(b"{\"schema_version\":1,\"id\":\"ev_trunc")
            .expect("write");
        let events = log.read_session(&s).expect("read");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn finds_events_by_prefix() {
        let dir = tempfile::tempdir().expect("tmp");
        let log = EventLog::open(dir.path(), true).expect("open");
        let s = SessionId::new();
        let e = event(&s, "findme");
        log.append(&e).expect("append");
        let found = log
            .find_event(&e.id.as_str()[..10])
            .expect("search")
            .expect("found");
        assert_eq!(found.id, e.id);
        assert!(log.find_event("ev_nope").expect("search").is_none());
    }

    #[test]
    fn unknown_session_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let log = EventLog::open(dir.path(), true).expect("open");
        assert!(
            log.read_session(&SessionId::new())
                .expect("read")
                .is_empty()
        );
        assert!(log.latest_session().expect("latest").is_none());
    }
}
