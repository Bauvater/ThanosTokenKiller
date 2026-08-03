//! Content addressed blob storage.
//!
//! * The file name *is* the hash, so identical content is stored once.
//! * Writes go to a temp file inside the same directory and are then renamed,
//!   so a crash can never leave a half written blob behind.
//! * Every read verifies the hash again; a silently corrupted file surfaces as
//!   [`ttk_core::Error::Integrity`] instead of as bad model input.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ttk_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Zstd(i32),
}

impl Compression {
    pub fn from_config(name: &str, level: i32) -> Self {
        match name {
            "zstd" => Compression::Zstd(level),
            _ => Compression::None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Compression::None => "bin",
            Compression::Zstd(_) => "zst",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Zstd(_) => "zstd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// `blake3:<hex>`
    pub hash: String,
    pub original_bytes: u64,
    pub stored_bytes: u64,
    /// False when the blob already existed (deduplicated).
    pub newly_written: bool,
}

pub struct BlobStore {
    root: PathBuf,
    compression: Compression,
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>, compression: Compression) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self { root, compression })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn hex_of(hash: &str) -> &str {
        hash.split_once(':').map(|(_, h)| h).unwrap_or(hash)
    }

    fn path_for(&self, hash: &str, compression: Compression) -> PathBuf {
        let hex = Self::hex_of(hash);
        // Two level fan out keeps directory sizes reasonable on every OS.
        self.root
            .join(&hex[..2])
            .join(format!("{hex}.{}", compression.extension()))
    }

    /// Locate an existing blob regardless of how it was compressed.
    fn existing_path(&self, hash: &str) -> Option<(PathBuf, Compression)> {
        for c in [Compression::Zstd(0), Compression::None] {
            let p = self.path_for(hash, c);
            if p.is_file() {
                return Some((p, c));
            }
        }
        None
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.existing_path(hash).is_some()
    }

    /// Store `bytes`, returning its content reference.
    pub fn put(&self, bytes: &[u8]) -> Result<BlobRef> {
        let hash = ttk_core::hash_bytes(bytes);
        if let Some((path, _)) = self.existing_path(&hash) {
            let stored = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            return Ok(BlobRef {
                hash,
                original_bytes: bytes.len() as u64,
                stored_bytes: stored,
                newly_written: false,
            });
        }

        let payload = match self.compression {
            Compression::None => bytes.to_vec(),
            Compression::Zstd(level) => zstd::encode_all(bytes, level)
                .map_err(|e| Error::storage(format!("zstd encode failed: {e}")))?,
        };

        let target = self.path_for(&hash, self.compression);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = self
            .root
            .join("tmp")
            .join(format!("{}.tmp", Self::hex_of(&hash)));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&payload)?;
            // Durability: without this a power loss can leave an empty file
            // that still passes the "exists" check.
            f.sync_all()?;
        }
        // Rename is atomic on NTFS and POSIX when both paths share a volume.
        // A concurrent writer may have won the race; the content is identical
        // either way, so treat an existing target as success.
        match fs::rename(&tmp, &target) {
            Ok(()) => {}
            Err(_) if target.is_file() => {
                let _ = fs::remove_file(&tmp);
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(Error::storage(format!(
                    "cannot move blob into place ({}): {e}",
                    target.display()
                )));
            }
        }

        Ok(BlobRef {
            hash,
            original_bytes: bytes.len() as u64,
            stored_bytes: payload.len() as u64,
            newly_written: true,
        })
    }

    pub fn put_str(&self, text: &str) -> Result<BlobRef> {
        self.put(text.as_bytes())
    }

    /// Read a blob and verify its hash.
    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let (path, compression) = self
            .existing_path(hash)
            .ok_or_else(|| Error::storage(format!("blob {hash} is missing")))?;
        let raw = fs::read(&path)?;
        let bytes = match compression {
            Compression::None => raw,
            Compression::Zstd(_) => zstd::decode_all(raw.as_slice())
                .map_err(|e| Error::storage(format!("zstd decode failed for {hash}: {e}")))?,
        };
        let actual = ttk_core::hash_bytes(&bytes);
        if actual != hash {
            return Err(Error::Integrity {
                what: format!("blob {}", path.display()),
                expected: hash.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn get_string(&self, hash: &str) -> Result<String> {
        let bytes = self.get(hash)?;
        String::from_utf8(bytes)
            .map_err(|e| Error::storage(format!("blob {hash} is not valid UTF-8: {e}")))
    }

    pub fn remove(&self, hash: &str) -> Result<bool> {
        match self.existing_path(hash) {
            Some((path, _)) => {
                fs::remove_file(path)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Total bytes on disk plus blob count.
    pub fn stats(&self) -> Result<(u64, u64)> {
        let mut bytes = 0u64;
        let mut count = 0u64;
        for shard in fs::read_dir(&self.root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name() == "tmp" {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    bytes += entry.metadata()?.len();
                    count += 1;
                }
            }
        }
        Ok((bytes, count))
    }

    /// Remove leftovers from interrupted writes.
    pub fn clean_tmp(&self) -> Result<u64> {
        let tmp = self.root.join("tmp");
        let mut removed = 0;
        if tmp.is_dir() {
            for e in fs::read_dir(&tmp)? {
                let e = e?;
                if e.file_type()?.is_file() && fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Hashes of every stored blob.
    pub fn list(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for shard in fs::read_dir(&self.root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name() == "tmp" {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some((hex, _)) = name.rsplit_once('.') {
                    out.push(format!("blake3:{hex}"));
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> BlobStore {
        BlobStore::open(dir.path().join("blobs"), Compression::Zstd(6)).expect("open")
    }

    #[test]
    fn roundtrips_bytes_exactly() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        let payload = "hello\r\n\0 binary-ish \u{1F600} content".repeat(50);
        let r = s.put_str(&payload).expect("put");
        assert!(r.newly_written);
        assert_eq!(s.get_string(&r.hash).expect("get"), payload);
    }

    #[test]
    fn deduplicates() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        let a = s.put_str("same").expect("put a");
        let b = s.put_str("same").expect("put b");
        assert_eq!(a.hash, b.hash);
        assert!(a.newly_written && !b.newly_written);
        assert_eq!(s.stats().expect("stats").1, 1);
    }

    #[test]
    fn detects_corruption() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = BlobStore::open(dir.path().join("blobs"), Compression::None).expect("open");
        let r = s.put_str("important output").expect("put");
        let (path, _) = s.existing_path(&r.hash).expect("path");
        fs::write(&path, b"tampered").expect("overwrite");
        let err = s.get(&r.hash).expect_err("must detect corruption");
        assert!(matches!(err, Error::Integrity { .. }), "{err}");
    }

    #[test]
    fn missing_blob_is_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        assert!(s.get("blake3:deadbeef").is_err());
        assert!(!s.contains("blake3:deadbeef"));
        assert!(!s.remove("blake3:deadbeef").expect("remove"));
    }

    #[test]
    fn empty_content_is_storable() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        let r = s.put_str("").expect("put");
        assert_eq!(s.get_string(&r.hash).expect("get"), "");
    }

    #[test]
    fn tmp_files_are_cleanable() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        fs::write(s.root().join("tmp").join("stale.tmp"), b"x").expect("write");
        assert_eq!(s.clean_tmp().expect("clean"), 1);
    }

    #[test]
    fn compression_actually_shrinks_repetitive_data() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        let payload = "the same line over and over\n".repeat(2000);
        let r = s.put_str(&payload).expect("put");
        assert!(
            r.stored_bytes * 10 < r.original_bytes,
            "stored {} vs original {}",
            r.stored_bytes,
            r.original_bytes
        );
    }
}
