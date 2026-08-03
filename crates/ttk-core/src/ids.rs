//! Monotonic, sortable, dependency-free identifiers.
//!
//! Layout (26 chars, Crockford base32, ULID-compatible ordering):
//! `48 bit` millisecond timestamp | `80 bit` entropy.
//!
//! The entropy half is derived from a process-unique seed plus a monotonic
//! counter, so ids are unique per process and strictly increasing inside a
//! process even when two are created within the same millisecond. That is
//! enough for a local, single-writer store and avoids pulling in an RNG.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn process_seed() -> u64 {
    use std::sync::OnceLock;
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let mut h = blake3::Hasher::new();
        h.update(&std::process::id().to_le_bytes());
        h.update(
            &SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        // Address of a stack local adds ASLR entropy on all supported platforms.
        let anchor = 0u8;
        h.update(&(&anchor as *const u8 as usize).to_le_bytes());
        let out = h.finalize();
        u64::from_le_bytes(out.as_bytes()[..8].try_into().expect("8 bytes"))
    })
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn encode(ms: u64, entropy: u128) -> String {
    // 128 bit value: timestamp in the high 48 bits.
    let value: u128 = ((ms as u128 & ((1u128 << 48) - 1)) << 80) | (entropy & ((1u128 << 80) - 1));
    let mut buf = [0u8; 26];
    // 26 * 5 = 130 bits; the top two bits are always zero.
    for (i, slot) in buf.iter_mut().enumerate() {
        let shift = 125 - (i * 5);
        let idx = ((value >> shift) & 0x1f) as usize;
        *slot = ALPHABET[idx];
    }
    // SAFETY-free: every byte came from ALPHABET which is ASCII.
    String::from_utf8(buf.to_vec()).expect("alphabet is ascii")
}

fn generate(prefix: &str) -> String {
    let ms = now_millis();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let entropy = ((process_seed() as u128) << 16) ^ (n as u128);
    format!("{prefix}{}", encode(ms, entropy))
}

macro_rules! id_type {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Create a new, sortable identifier.
            pub fn new() -> Self {
                Self(generate($prefix))
            }

            /// Wrap an existing string (used when reading from storage).
            pub fn from_string(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Millisecond timestamp encoded in the id, if it is well formed.
            pub fn timestamp_millis(&self) -> Option<u64> {
                let body = self.0.strip_prefix($prefix)?;
                if body.len() != 26 {
                    return None;
                }
                let mut value: u128 = 0;
                for b in body.bytes() {
                    let idx = ALPHABET.iter().position(|&c| c == b)? as u128;
                    value = (value << 5) | idx;
                }
                Some((value >> 80) as u64)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}

id_type!(
    EventId,
    "ev_",
    "Identifier of a single [`crate::event::TokenEvent`]."
);
id_type!(SessionId, "se_", "Identifier of a capture session.");
id_type!(CapsuleId, "cap_", "Identifier of a stored capsule.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_monotonic() {
        let a = EventId::new();
        let b = EventId::new();
        assert_ne!(a, b);
        assert!(a < b, "{a} should sort before {b}");
        assert_eq!(a.as_str().len(), 3 + 26);
    }

    #[test]
    fn timestamp_roundtrips() {
        let before = now_millis();
        let id = CapsuleId::new();
        let ts = id.timestamp_millis().expect("valid id");
        assert!(ts >= before && ts <= now_millis() + 1_000);
    }

    #[test]
    fn encode_is_stable() {
        assert_eq!(encode(0, 0), "0".repeat(26));
        assert_eq!(encode(1, 0).len(), 26);
    }
}
