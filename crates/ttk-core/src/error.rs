//! Error type shared by all ThanosTokenKiller crates.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("capsule `{0}` not found")]
    CapsuleNotFound(String),

    #[error("integrity check failed for {what}: expected {expected}, got {actual}")]
    Integrity {
        what: String,
        expected: String,
        actual: String,
    },

    #[error("parse error in {what}: {message}")]
    Parse { what: String, message: String },

    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("access denied: {0}")]
    AccessDenied(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn storage(msg: impl std::fmt::Display) -> Self {
        Error::Storage(msg.to_string())
    }

    pub fn parse(what: &str, msg: impl std::fmt::Display) -> Self {
        Error::Parse {
            what: what.to_string(),
            message: msg.to_string(),
        }
    }

    pub fn other(msg: impl std::fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }

    /// Exit code for the CLI. Distinct codes make scripting possible.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) => 2,
            Error::Io(_) => 3,
            Error::Storage(_) => 4,
            Error::CapsuleNotFound(_) => 5,
            Error::Integrity { .. } => 6,
            Error::Parse { .. } => 7,
            Error::LimitExceeded(_) => 8,
            Error::AccessDenied(_) => 9,
            Error::Other(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct_and_nonzero() {
        let errs = [
            Error::Config("x".into()),
            Error::Storage("x".into()),
            Error::CapsuleNotFound("x".into()),
            Error::LimitExceeded("x".into()),
            Error::AccessDenied("x".into()),
            Error::Other("x".into()),
        ];
        let mut codes: Vec<i32> = errs.iter().map(|e| e.exit_code()).collect();
        assert!(codes.iter().all(|&c| c != 0));
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), errs.len());
    }
}
