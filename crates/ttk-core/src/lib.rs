//! ThanosTokenKiller core.
//!
//! *The smallest provably sufficient context.*
//!
//! This crate owns the data model and the safety rules. It performs no I/O
//! except reading configuration files, and it never talks to the network.
//!
//! The three load bearing ideas:
//!
//! * [`event::TokenEvent`] — every byte that flows through the system is one
//!   event with provenance, trust label and token accounting.
//! * [`ir`] — compilers emit a compact, re-parsable intermediate
//!   representation instead of free prose.
//! * [`firewall`] — nothing replaces the original unless declared invariants
//!   survive verbatim, the structure re-parses and the change actually saves
//!   tokens.

pub mod config;
pub mod content;
pub mod error;
pub mod event;
pub mod firewall;
pub mod ids;
pub mod invariants;
pub mod ir;
pub mod redact;
pub mod tokens;
pub mod trust;

pub use error::{Error, Result};

/// Canonical content hash: `blake3:<64 hex chars>`.
///
/// BLAKE3 is used everywhere (blobs, capsules, cache keys) so a hash can be
/// compared across subsystems without conversion.
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

pub fn hash_string(s: &str) -> String {
    hash_bytes(s.as_bytes())
}

/// Short, human friendly form of a hash for reports (`blake3:1a2b3c4d`).
pub fn short_hash(hash: &str) -> String {
    match hash.split_once(':') {
        Some((algo, hex)) => {
            let short = &hex[..hex.len().min(8)];
            format!("{algo}:{short}")
        }
        None => hash.chars().take(8).collect(),
    }
}

/// Version of the whole toolchain, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable_and_prefixed() {
        let a = hash_string("hello");
        assert_eq!(a, hash_string("hello"));
        assert_ne!(a, hash_string("hello "));
        assert!(a.starts_with("blake3:"));
        assert_eq!(a.len(), "blake3:".len() + 64);
        assert_eq!(short_hash(&a).len(), "blake3:".len() + 8);
    }

    #[test]
    fn empty_input_hashes() {
        assert!(hash_bytes(&[]).starts_with("blake3:"));
        assert_eq!(short_hash("abc"), "abc");
    }
}
