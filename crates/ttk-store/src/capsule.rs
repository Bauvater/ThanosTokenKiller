//! Reversible multi-resolution capsules.
//!
//! Every transformation that removes information must first write a capsule.
//! A capsule keeps the byte exact original (level 4) plus progressively
//! smaller renderings, so an agent can always retrieve exactly as much detail
//! as it needs:
//!
//! | level | content                                   |
//! |-------|-------------------------------------------|
//! | 0     | one line                                  |
//! | 1     | key results                               |
//! | 2     | relevant sections                         |
//! | 3     | full structured rendering (the Token IR)  |
//! | 4     | byte exact original                       |
//!
//! Levels 0-3 are small and live in the metadata database; level 4 lives in
//! the content addressed blob store.

use std::collections::BTreeMap;
use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use ttk_core::content::ContentType;
use ttk_core::ids::{CapsuleId, EventId};
use ttk_core::invariants::Invariant;
use ttk_core::trust::{SensitivityLevel, TrustLevel};
use ttk_core::{Error, Result};

use crate::blob::BlobStore;

pub const CAPSULE_SCHEMA_VERSION: u32 = 1;

const CAPSULES: TableDefinition<&str, &[u8]> = TableDefinition::new("capsules");
const BY_HASH: TableDefinition<&str, &str> = TableDefinition::new("capsules_by_source_hash");
/// `short handle -> source hash`. Deliberately not `-> id`: the handle is a
/// property of the *content*, so the ownership check when allocating one is a
/// direct comparison instead of a capsule load.
const BY_SHORT: TableDefinition<&str, &str> = TableDefinition::new("capsules_by_short");

/// Starting length of a short handle, in hex characters.
///
/// A full capsule id is `cap_` plus 26 base32 characters of timestamp and
/// entropy. Random alphanumerics tokenise appallingly — that reference costs
/// around thirteen tokens, and it is printed on *every* compiled output. On a
/// `cargo test` that compiles down to 62 tokens, the pointer to the original is
/// a fifth of the whole message. Five hex characters cost two or three tokens
/// and are just as unambiguous inside one workspace.
const SHORT_LEN: usize = 5;

/// Longest a short handle grows before giving up and using the full id.
const SHORT_MAX_LEN: usize = 12;

/// Highest detail level. Always the untouched original.
pub const LEVEL_RAW: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capsule {
    pub schema_version: u32,
    pub id: CapsuleId,
    /// Short, unambiguous handle for this workspace: what compiled output
    /// prints, and what `ttk retrieve` accepts. Derived from the content hash,
    /// so identical output always gets the same handle — which is a useful
    /// signal in its own right when a command is run twice.
    ///
    /// Empty on records written before short handles existed; those still
    /// resolve by their full id.
    #[serde(default)]
    pub short: String,
    pub source_event: Option<EventId>,
    pub content_type: ContentType,
    /// Hash of the original content; also the blob key for level 4.
    pub source_hash: String,
    pub trust_level: TrustLevel,
    pub sensitivity: SensitivityLevel,
    pub created_millis: u64,
    pub expires_millis: Option<u64>,
    pub compression: String,
    /// Levels 0..=3.
    pub levels: BTreeMap<u8, String>,
    pub original_bytes: u64,
    pub stored_bytes: u64,
    pub invariants: Vec<Invariant>,
    /// Placeholder → secret mapping, stored locally only.
    #[serde(default)]
    pub secret_mapping: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl Capsule {
    /// `cap://<short>` if this capsule has a short handle, else `cap://<id>`.
    pub fn reference(&self) -> String {
        if self.short.is_empty() {
            format!("cap://{}", self.id)
        } else {
            format!("cap://{}", self.short)
        }
    }

    /// Best available rendering at or below `level`.
    pub fn level_text(&self, level: u8) -> Option<&str> {
        (0..=level.min(3))
            .rev()
            .find_map(|l| self.levels.get(&l))
            .map(String::as_str)
    }

    pub fn is_expired(&self, now_millis: u64) -> bool {
        self.expires_millis.is_some_and(|e| e <= now_millis)
    }

    pub fn requires_raw_gate(&self) -> bool {
        self.sensitivity == SensitivityLevel::Secret
    }
}

/// Builder used by the pipeline.
#[derive(Debug, Clone)]
pub struct NewCapsule {
    pub content: String,
    pub content_type: ContentType,
    pub source_event: Option<EventId>,
    pub trust_level: TrustLevel,
    pub sensitivity: SensitivityLevel,
    pub levels: BTreeMap<u8, String>,
    pub invariants: Vec<Invariant>,
    pub secret_mapping: BTreeMap<String, String>,
    pub metadata: Map<String, Value>,
    pub ttl_days: u32,
}

impl NewCapsule {
    pub fn new(content: impl Into<String>, content_type: ContentType) -> Self {
        Self {
            content: content.into(),
            content_type,
            source_event: None,
            trust_level: TrustLevel::ToolStructured,
            sensitivity: SensitivityLevel::Internal,
            levels: BTreeMap::new(),
            invariants: Vec::new(),
            secret_mapping: BTreeMap::new(),
            metadata: Map::new(),
            ttl_days: 14,
        }
    }

    pub fn level(mut self, level: u8, text: impl Into<String>) -> Self {
        debug_assert!(level <= 3, "levels 0..=3 are inline, level 4 is the blob");
        self.levels.insert(level.min(3), text.into());
        self
    }

    pub fn event(mut self, id: EventId) -> Self {
        self.source_event = Some(id);
        self
    }

    pub fn trust(mut self, t: TrustLevel) -> Self {
        self.trust_level = t;
        self
    }

    pub fn sensitivity(mut self, s: SensitivityLevel) -> Self {
        self.sensitivity = self.sensitivity.max(s);
        self
    }

    pub fn invariants(mut self, inv: Vec<Invariant>) -> Self {
        self.invariants = inv;
        self
    }

    pub fn secret_mapping(mut self, m: BTreeMap<String, String>) -> Self {
        if !m.is_empty() {
            self.sensitivity = self.sensitivity.max(SensitivityLevel::Secret);
        }
        self.secret_mapping = m;
        self
    }

    pub fn meta(mut self, k: &str, v: Value) -> Self {
        self.metadata.insert(k.to_string(), v);
        self
    }

    pub fn ttl_days(mut self, days: u32) -> Self {
        self.ttl_days = days;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcStats {
    pub expired_removed: u64,
    pub evicted_for_size: u64,
    pub blobs_removed: u64,
    pub bytes_freed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStats {
    pub capsules: u64,
    pub blob_bytes: u64,
    pub blob_count: u64,
    pub original_bytes: u64,
}

pub struct CapsuleStore {
    db: Database,
    blobs: BlobStore,
    max_storage_bytes: u64,
}

impl CapsuleStore {
    pub fn open(index_path: &Path, blobs: BlobStore, max_storage_gb: f64) -> Result<Self> {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::create(index_path)
            .map_err(|e| Error::storage(format!("cannot open capsule index: {e}")))?;
        // Create the tables up front so read transactions never fail on a
        // fresh store.
        {
            let tx = db
                .begin_write()
                .map_err(|e| Error::storage(format!("index write txn: {e}")))?;
            tx.open_table(CAPSULES)
                .map_err(|e| Error::storage(format!("open capsules table: {e}")))?;
            tx.open_table(BY_HASH)
                .map_err(|e| Error::storage(format!("open index table: {e}")))?;
            tx.open_table(BY_SHORT)
                .map_err(|e| Error::storage(format!("open short index table: {e}")))?;
            tx.commit()
                .map_err(|e| Error::storage(format!("index commit: {e}")))?;
        }
        Ok(Self {
            db,
            blobs,
            max_storage_bytes: (max_storage_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64,
        })
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    /// Store a capsule. Identical content reuses the existing blob.
    pub fn put(&self, new: NewCapsule) -> Result<Capsule> {
        let blob = self.blobs.put_str(&new.content)?;
        let now = ttk_core::ids::now_millis();
        let short = self.allocate_short(&blob.hash)?;
        let capsule = Capsule {
            schema_version: CAPSULE_SCHEMA_VERSION,
            id: CapsuleId::new(),
            short,
            source_event: new.source_event,
            content_type: new.content_type,
            source_hash: blob.hash.clone(),
            trust_level: new.trust_level,
            sensitivity: new.sensitivity,
            created_millis: now,
            expires_millis: if new.ttl_days == 0 {
                None
            } else {
                Some(now + new.ttl_days as u64 * 86_400_000)
            },
            compression: self.blobs_compression(),
            levels: new.levels,
            original_bytes: blob.original_bytes,
            stored_bytes: blob.stored_bytes,
            invariants: new.invariants,
            secret_mapping: new.secret_mapping,
            metadata: new.metadata,
        };
        self.write_capsule(&capsule)?;
        Ok(capsule)
    }

    /// Pick the shortest unambiguous handle for this content.
    ///
    /// Derived from the content hash rather than the id, so re-running a
    /// command that produces the same bytes yields the same handle. Grows by
    /// two characters at a time on collision and falls back to the empty string
    /// — meaning "use the full id" — rather than ever returning an ambiguous
    /// one.
    fn allocate_short(&self, source_hash: &str) -> Result<String> {
        let hex = source_hash.strip_prefix("blake3:").unwrap_or(source_hash);
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::storage(format!("read txn: {e}")))?;
        let t = tx
            .open_table(BY_SHORT)
            .map_err(|e| Error::storage(format!("open short index: {e}")))?;

        let mut len = SHORT_LEN;
        while len <= SHORT_MAX_LEN.min(hex.len()) {
            let candidate = &hex[..len];
            match t
                .get(candidate)
                .map_err(|e| Error::storage(format!("get short: {e}")))?
            {
                // Free, or already pointing at this very content.
                None => return Ok(candidate.to_string()),
                Some(existing) if existing.value() == source_hash => {
                    return Ok(candidate.to_string());
                }
                Some(_) => {}
            }
            len += 2;
        }
        Ok(String::new())
    }

    fn blobs_compression(&self) -> String {
        // The blob store knows its own setting; capsules record it for audit.
        "zstd".to_string()
    }

    fn write_capsule(&self, capsule: &Capsule) -> Result<()> {
        let bytes = serde_json::to_vec(capsule)
            .map_err(|e| Error::storage(format!("cannot serialize capsule: {e}")))?;
        let tx = self
            .db
            .begin_write()
            .map_err(|e| Error::storage(format!("write txn: {e}")))?;
        {
            let mut t = tx
                .open_table(CAPSULES)
                .map_err(|e| Error::storage(format!("open table: {e}")))?;
            t.insert(capsule.id.as_str(), bytes.as_slice())
                .map_err(|e| Error::storage(format!("insert capsule: {e}")))?;
            let mut h = tx
                .open_table(BY_HASH)
                .map_err(|e| Error::storage(format!("open table: {e}")))?;
            h.insert(capsule.source_hash.as_str(), capsule.id.as_str())
                .map_err(|e| Error::storage(format!("insert hash index: {e}")))?;
            if !capsule.short.is_empty() {
                let mut sh = tx
                    .open_table(BY_SHORT)
                    .map_err(|e| Error::storage(format!("open short table: {e}")))?;
                sh.insert(capsule.short.as_str(), capsule.source_hash.as_str())
                    .map_err(|e| Error::storage(format!("insert short index: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| Error::storage(format!("commit: {e}")))?;
        Ok(())
    }

    /// Look up a capsule by id, short handle or unambiguous id prefix.
    ///
    /// `cap://` is accepted and ignored, so anything printed in compiled output
    /// can be pasted straight back.
    pub fn get(&self, id: &str) -> Result<Capsule> {
        let id = id.trim().trim_start_matches("cap://");
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::storage(format!("read txn: {e}")))?;
        let t = tx
            .open_table(CAPSULES)
            .map_err(|e| Error::storage(format!("open table: {e}")))?;
        if let Some(v) = t
            .get(id)
            .map_err(|e| Error::storage(format!("get capsule: {e}")))?
        {
            return serde_json::from_slice(v.value())
                .map_err(|e| Error::storage(format!("corrupt capsule record: {e}")));
        }

        // A short handle: two exact index lookups, short -> hash -> id. Tried
        // before the prefix scan because it is the reference compiled output
        // actually prints, so it is the one people paste back.
        let short = tx
            .open_table(BY_SHORT)
            .map_err(|e| Error::storage(format!("open short index: {e}")))?;
        let by_hash = tx
            .open_table(BY_HASH)
            .map_err(|e| Error::storage(format!("open hash index: {e}")))?;
        if let Some(hash) = short
            .get(id)
            .map_err(|e| Error::storage(format!("get short: {e}")))?
            && let Some(target) = by_hash
                .get(hash.value())
                .map_err(|e| Error::storage(format!("get by hash: {e}")))?
            && let Some(v) = t
                .get(target.value())
                .map_err(|e| Error::storage(format!("get capsule: {e}")))?
        {
            return serde_json::from_slice(v.value())
                .map_err(|e| Error::storage(format!("corrupt capsule record: {e}")));
        }

        // Prefix lookup, but only when it is unambiguous.
        let mut found: Option<Vec<u8>> = None;
        for entry in t
            .iter()
            .map_err(|e| Error::storage(format!("scan capsules: {e}")))?
        {
            let (k, v) = entry.map_err(|e| Error::storage(format!("scan capsules: {e}")))?;
            if k.value().starts_with(id) {
                if found.is_some() {
                    return Err(Error::CapsuleNotFound(format!("{id} (ambiguous prefix)")));
                }
                found = Some(v.value().to_vec());
            }
        }
        match found {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::storage(format!("corrupt capsule record: {e}"))),
            None => Err(Error::CapsuleNotFound(id.to_string())),
        }
    }

    pub fn find_by_source_hash(&self, hash: &str) -> Result<Option<Capsule>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::storage(format!("read txn: {e}")))?;
        let h = tx
            .open_table(BY_HASH)
            .map_err(|e| Error::storage(format!("open table: {e}")))?;
        let Some(id) = h
            .get(hash)
            .map_err(|e| Error::storage(format!("get: {e}")))?
        else {
            return Ok(None);
        };
        let id = id.value().to_string();
        drop(h);
        drop(tx);
        self.get(&id).map(Some)
    }

    /// Byte exact original (level 4).
    ///
    /// `allow_secret` must be true to read a capsule that contains detected
    /// secrets; the caller is responsible for asking the user.
    pub fn raw(&self, id: &str, allow_secret: bool) -> Result<Vec<u8>> {
        let capsule = self.get(id)?;
        if capsule.requires_raw_gate() && !allow_secret {
            return Err(Error::AccessDenied(format!(
                "capsule {} contains detected secrets; re-run with --allow-secrets to read it",
                capsule.id
            )));
        }
        self.blobs.get(&capsule.source_hash)
    }

    /// Rendering at `level`. Level 4 returns the original text.
    pub fn render(&self, id: &str, level: u8, allow_secret: bool) -> Result<String> {
        let capsule = self.get(id)?;
        if level >= LEVEL_RAW {
            if capsule.requires_raw_gate() && !allow_secret {
                return Err(Error::AccessDenied(format!(
                    "capsule {} contains detected secrets; re-run with --allow-secrets to read it",
                    capsule.id
                )));
            }
            return self.blobs.get_string(&capsule.source_hash);
        }
        match capsule.level_text(level) {
            Some(text) => Ok(text.to_string()),
            // No inline rendering was stored: fall back to the original, which
            // is always correct, never silently empty.
            None => self.blobs.get_string(&capsule.source_hash),
        }
    }

    /// A 1-based, inclusive line range of the original.
    pub fn lines(&self, id: &str, from: usize, to: usize, allow_secret: bool) -> Result<String> {
        let text = self.render(id, LEVEL_RAW, allow_secret)?;
        let from = from.max(1);
        Ok(text
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let n = i + 1;
                n >= from && n <= to
            })
            .map(|(i, l)| format!("{}: {l}", i + 1))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Literal, case insensitive search inside the original.
    pub fn search(
        &self,
        id: &str,
        needle: &str,
        max_hits: usize,
        allow_secret: bool,
    ) -> Result<Vec<(usize, String)>> {
        let text = self.render(id, LEVEL_RAW, allow_secret)?;
        let needle_lower = needle.to_lowercase();
        Ok(text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&needle_lower))
            .take(max_hits)
            .map(|(i, l)| (i + 1, l.to_string()))
            .collect())
    }

    pub fn list(&self, limit: usize) -> Result<Vec<Capsule>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::storage(format!("read txn: {e}")))?;
        let t = tx
            .open_table(CAPSULES)
            .map_err(|e| Error::storage(format!("open table: {e}")))?;
        let mut out = Vec::new();
        // Ids are time sortable, so reverse iteration yields newest first.
        for entry in t
            .iter()
            .map_err(|e| Error::storage(format!("scan: {e}")))?
            .rev()
        {
            let (_, v) = entry.map_err(|e| Error::storage(format!("scan: {e}")))?;
            if let Ok(c) = serde_json::from_slice::<Capsule>(v.value()) {
                out.push(c);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let capsule = match self.get(id) {
            Ok(c) => c,
            Err(Error::CapsuleNotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        let tx = self
            .db
            .begin_write()
            .map_err(|e| Error::storage(format!("write txn: {e}")))?;
        {
            let mut t = tx
                .open_table(CAPSULES)
                .map_err(|e| Error::storage(format!("open table: {e}")))?;
            t.remove(capsule.id.as_str())
                .map_err(|e| Error::storage(format!("remove: {e}")))?;
        }
        tx.commit()
            .map_err(|e| Error::storage(format!("commit: {e}")))?;
        self.drop_unreferenced_blob(&capsule.source_hash)?;
        Ok(true)
    }

    /// Remove a blob when no capsule references it any more.
    fn drop_unreferenced_blob(&self, hash: &str) -> Result<bool> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::storage(format!("read txn: {e}")))?;
        let t = tx
            .open_table(CAPSULES)
            .map_err(|e| Error::storage(format!("open table: {e}")))?;
        for entry in t.iter().map_err(|e| Error::storage(format!("scan: {e}")))? {
            let (_, v) = entry.map_err(|e| Error::storage(format!("scan: {e}")))?;
            if let Ok(c) = serde_json::from_slice::<Capsule>(v.value())
                && c.source_hash == hash
            {
                return Ok(false);
            }
        }
        drop(t);
        drop(tx);
        self.blobs.remove(hash)
    }

    /// Drop expired capsules, then evict oldest capsules until the store fits
    /// into `max_storage_gb`.
    pub fn gc(&self, now_millis: u64) -> Result<GcStats> {
        let mut stats = GcStats::default();
        let all = self.list(usize::MAX)?;

        for capsule in all.iter().filter(|c| c.is_expired(now_millis)) {
            if self.delete(capsule.id.as_str())? {
                stats.expired_removed += 1;
                stats.bytes_freed += capsule.stored_bytes;
            }
        }

        if self.max_storage_bytes > 0 {
            let (mut bytes, _) = self.blobs.stats()?;
            if bytes > self.max_storage_bytes {
                // Oldest first.
                let mut remaining: Vec<&Capsule> =
                    all.iter().filter(|c| !c.is_expired(now_millis)).collect();
                remaining.sort_by_key(|c| c.created_millis);
                for capsule in remaining {
                    if bytes <= self.max_storage_bytes {
                        break;
                    }
                    if self.delete(capsule.id.as_str())? {
                        stats.evicted_for_size += 1;
                        stats.bytes_freed += capsule.stored_bytes;
                        bytes = bytes.saturating_sub(capsule.stored_bytes);
                    }
                }
            }
        }

        stats.blobs_removed = stats.expired_removed + stats.evicted_for_size;
        self.blobs.clean_tmp()?;
        Ok(stats)
    }

    pub fn stats(&self) -> Result<StoreStats> {
        let (blob_bytes, blob_count) = self.blobs.stats()?;
        let capsules = self.list(usize::MAX)?;
        Ok(StoreStats {
            capsules: capsules.len() as u64,
            blob_bytes,
            blob_count,
            original_bytes: capsules.iter().map(|c| c.original_bytes).sum(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::Compression;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: CapsuleStore,
    }

    fn fixture(max_gb: f64) -> Fixture {
        let dir = tempfile::tempdir().expect("tmp");
        let blobs = BlobStore::open(dir.path().join("blobs"), Compression::Zstd(3)).expect("blobs");
        let store =
            CapsuleStore::open(&dir.path().join("index.redb"), blobs, max_gb).expect("store");
        Fixture { _dir: dir, store }
    }

    fn sample(content: &str) -> NewCapsule {
        NewCapsule::new(content, ContentType::TestOutput)
            .level(0, "1 test failed")
            .level(1, "FAILED test_expiry")
    }

    #[test]
    fn put_and_retrieve_every_level() {
        let f = fixture(1.0);
        let content = "line one\nline two\nline three\n";
        let c = f.store.put(sample(content)).expect("put");

        assert_eq!(
            f.store.render(c.id.as_str(), 0, false).unwrap(),
            "1 test failed"
        );
        assert_eq!(
            f.store.render(c.id.as_str(), 1, false).unwrap(),
            "FAILED test_expiry"
        );
        // Level 2 has no rendering of its own, so it degrades to level 1.
        assert_eq!(
            f.store.render(c.id.as_str(), 2, false).unwrap(),
            "FAILED test_expiry"
        );
        assert_eq!(f.store.render(c.id.as_str(), 4, false).unwrap(), content);
        assert_eq!(
            f.store.raw(c.id.as_str(), false).unwrap(),
            content.as_bytes()
        );
    }

    #[test]
    fn roundtrip_is_byte_exact() {
        let f = fixture(1.0);
        let content = "weird\r\ncontent \u{1F600}\ttabs\n\nand trailing spaces   \n";
        let c = f
            .store
            .put(NewCapsule::new(content, ContentType::Unknown))
            .expect("put");
        assert_eq!(
            f.store.render(c.id.as_str(), LEVEL_RAW, false).unwrap(),
            content
        );
    }

    #[test]
    fn a_short_handle_is_stable_for_identical_content() {
        let f = fixture(1.0);
        let a = f
            .store
            .put(NewCapsule::new("same bytes", ContentType::PlainText))
            .expect("put a");
        let b = f
            .store
            .put(NewCapsule::new("same bytes", ContentType::PlainText))
            .expect("put b");
        assert!(!a.short.is_empty());
        assert_eq!(
            a.short, b.short,
            "identical output gets an identical handle, which is a signal in itself"
        );
        assert_ne!(a.id, b.id, "but they are still separate capsules");

        let other = f
            .store
            .put(NewCapsule::new("other bytes", ContentType::PlainText))
            .expect("put other");
        assert_ne!(a.short, other.short);
    }

    #[test]
    fn a_short_handle_is_much_cheaper_than_an_id() {
        let f = fixture(1.0);
        let c = f
            .store
            .put(NewCapsule::new("payload", ContentType::PlainText))
            .expect("put");
        assert!(
            c.reference().len() < format!("cap://{}", c.id).len() / 2,
            "{} vs cap://{}",
            c.reference(),
            c.id
        );
        assert!(c.reference().starts_with("cap://"));
    }

    #[test]
    fn a_capsule_resolves_by_its_short_handle() {
        let f = fixture(1.0);
        let c = f
            .store
            .put(NewCapsule::new("payload", ContentType::PlainText))
            .expect("put");
        assert_eq!(f.store.get(&c.short).expect("by short").id, c.id);
        assert_eq!(f.store.get(&c.reference()).expect("by ref").id, c.id);
        assert_eq!(f.store.get(c.id.as_str()).expect("by id").id, c.id);
    }

    #[test]
    fn a_record_without_a_short_handle_still_resolves() {
        let f = fixture(1.0);
        let mut c = f
            .store
            .put(NewCapsule::new("payload", ContentType::PlainText))
            .expect("put");
        c.short = String::new();
        assert_eq!(c.reference(), format!("cap://{}", c.id));
    }

    #[test]
    fn accepts_cap_url_and_prefix() {
        let f = fixture(1.0);
        let c = f.store.put(sample("x")).expect("put");
        assert!(f.store.get(&format!("cap://{}", c.id)).is_ok());
        let prefix = &c.id.as_str()[..12];
        assert_eq!(f.store.get(prefix).expect("prefix").id, c.id);
        assert!(matches!(
            f.store.get("cap_doesnotexist"),
            Err(Error::CapsuleNotFound(_))
        ));
    }

    #[test]
    fn secrets_gate_raw_access() {
        let f = fixture(1.0);
        let mut mapping = BTreeMap::new();
        mapping.insert("<secret:token_1>".to_string(), "hunter2".to_string());
        let c = f
            .store
            .put(sample("Authorization: Bearer hunter2").secret_mapping(mapping))
            .expect("put");
        assert_eq!(c.sensitivity, SensitivityLevel::Secret);
        assert!(matches!(
            f.store.raw(c.id.as_str(), false),
            Err(Error::AccessDenied(_))
        ));
        assert!(f.store.raw(c.id.as_str(), true).is_ok());
        // Low levels stay readable.
        assert!(f.store.render(c.id.as_str(), 0, false).is_ok());
    }

    #[test]
    fn search_and_lines() {
        let f = fixture(1.0);
        let content = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
        let c = f
            .store
            .put(NewCapsule::new(content, ContentType::ShellOutput))
            .expect("put");
        let hits = f
            .store
            .search(c.id.as_str(), "LINE 7", 10, false)
            .expect("search");
        assert_eq!(hits, vec![(7, "line 7".to_string())]);
        let range = f.store.lines(c.id.as_str(), 3, 5, false).expect("lines");
        assert_eq!(range, "3: line 3\n4: line 4\n5: line 5");
    }

    #[test]
    fn deduplicates_blobs_across_capsules() {
        let f = fixture(1.0);
        let a = f.store.put(sample("identical")).expect("a");
        let b = f.store.put(sample("identical")).expect("b");
        assert_ne!(a.id, b.id);
        assert_eq!(a.source_hash, b.source_hash);
        assert_eq!(f.store.stats().expect("stats").blob_count, 1);

        // Deleting one capsule must not orphan the other's data.
        assert!(f.store.delete(a.id.as_str()).expect("delete"));
        assert_eq!(
            f.store
                .render(b.id.as_str(), LEVEL_RAW, false)
                .expect("still there"),
            "identical"
        );
    }

    #[test]
    fn gc_removes_expired_capsules() {
        let f = fixture(1.0);
        let c = f.store.put(sample("old data").ttl_days(1)).expect("put");
        let future = ttk_core::ids::now_millis() + 2 * 86_400_000;
        let stats = f.store.gc(future).expect("gc");
        assert_eq!(stats.expired_removed, 1);
        assert!(matches!(
            f.store.get(c.id.as_str()),
            Err(Error::CapsuleNotFound(_))
        ));
        assert_eq!(f.store.stats().expect("stats").blob_count, 0);
    }

    #[test]
    fn ttl_zero_means_keep_forever() {
        let f = fixture(1.0);
        let c = f.store.put(sample("keep").ttl_days(0)).expect("put");
        assert!(c.expires_millis.is_none());
        let stats = f.store.gc(u64::MAX / 2).expect("gc");
        assert_eq!(stats.expired_removed, 0);
    }

    #[test]
    fn size_cap_evicts_oldest() {
        let f = fixture(0.000000001); // ~1 byte: everything is over budget
        let first = f
            .store
            .put(NewCapsule::new("a".repeat(4096), ContentType::Unknown).ttl_days(0))
            .expect("first");
        let _second = f
            .store
            .put(NewCapsule::new("b".repeat(4096), ContentType::Unknown).ttl_days(0))
            .expect("second");
        let stats = f.store.gc(ttk_core::ids::now_millis()).expect("gc");
        assert!(stats.evicted_for_size >= 1, "{stats:?}");
        assert!(f.store.get(first.id.as_str()).is_err());
    }

    #[test]
    fn find_by_source_hash() {
        let f = fixture(1.0);
        let c = f.store.put(sample("unique content here")).expect("put");
        let found = f
            .store
            .find_by_source_hash(&c.source_hash)
            .expect("query")
            .expect("some");
        assert_eq!(found.id, c.id);
        assert!(
            f.store
                .find_by_source_hash("blake3:nope")
                .expect("query")
                .is_none()
        );
    }

    #[test]
    fn reopening_keeps_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let index = dir.path().join("index.redb");
        let id = {
            let blobs = BlobStore::open(dir.path().join("blobs"), Compression::Zstd(3)).unwrap();
            let store = CapsuleStore::open(&index, blobs, 1.0).unwrap();
            store.put(sample("persisted")).expect("put").id
        };
        let blobs = BlobStore::open(dir.path().join("blobs"), Compression::Zstd(3)).unwrap();
        let store = CapsuleStore::open(&index, blobs, 1.0).unwrap();
        assert_eq!(
            store
                .render(id.as_str(), LEVEL_RAW, false)
                .expect("read back"),
            "persisted"
        );
    }
}
