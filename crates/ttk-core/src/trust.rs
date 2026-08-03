//! Trust and sensitivity labels.
//!
//! Labels are attached at capture time and must survive every transformation.
//! The quality firewall rejects any transformation that drops or *raises* a
//! trust label.

use serde::{Deserialize, Serialize};

/// Where content came from, ordered from most to least trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Content produced by ThanosTokenKiller or the host agent itself.
    SystemTrusted,
    /// Typed by the user in this session.
    UserTrusted,
    /// Checked-in project configuration the user controls.
    ProjectTrusted,
    /// Structured output of a tool we parsed ourselves.
    ToolStructured,
    /// Files and command output from a repository that may contain
    /// attacker-controlled text.
    RepositoryUntrusted,
    /// Anything fetched from the network.
    WebUntrusted,
    /// Model output that has not been verified.
    GeneratedUnverified,
}

impl TrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustLevel::SystemTrusted => "system_trusted",
            TrustLevel::UserTrusted => "user_trusted",
            TrustLevel::ProjectTrusted => "project_trusted",
            TrustLevel::ToolStructured => "tool_structured",
            TrustLevel::RepositoryUntrusted => "repository_untrusted",
            TrustLevel::WebUntrusted => "web_untrusted",
            TrustLevel::GeneratedUnverified => "generated_unverified",
        }
    }

    /// Untrusted content must never be treated as instructions.
    pub fn is_untrusted(self) -> bool {
        matches!(
            self,
            TrustLevel::RepositoryUntrusted
                | TrustLevel::WebUntrusted
                | TrustLevel::GeneratedUnverified
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevel {
    Public,
    Internal,
    Confidential,
    /// Contains detected secrets. Raw access is gated and audit-logged.
    Secret,
}

impl SensitivityLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            SensitivityLevel::Public => "public",
            SensitivityLevel::Internal => "internal",
            SensitivityLevel::Confidential => "confidential",
            SensitivityLevel::Secret => "secret",
        }
    }

    pub fn max(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_trust_descending() {
        assert!(TrustLevel::SystemTrusted < TrustLevel::WebUntrusted);
        assert!(TrustLevel::WebUntrusted.is_untrusted());
        assert!(!TrustLevel::ProjectTrusted.is_untrusted());
    }

    #[test]
    fn sensitivity_max() {
        assert_eq!(
            SensitivityLevel::Public.max(SensitivityLevel::Secret),
            SensitivityLevel::Secret
        );
    }
}
