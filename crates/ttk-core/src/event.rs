//! The central data model: every piece of content that flows through
//! ThanosTokenKiller becomes exactly one [`TokenEvent`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::content::ContentType;
use crate::ids::{CapsuleId, EventId, SessionId};
use crate::invariants::Invariant;
use crate::tokens::TokenCount;
use crate::trust::{SensitivityLevel, TrustLevel};

/// Schema version of the serialized event. Bumped on breaking changes so old
/// JSONL logs stay replayable.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    SystemPrompt,
    UserPrompt,
    AssistantOutput,
    ToolSchema,
    ToolCall,
    ToolResult,
    ShellCommand,
    ShellOutput,
    FileContent,
    GitOutput,
    TestOutput,
    LogOutput,
    McpMessage,
    Memory,
    SubagentMessage,
    RetrievedCapsule,
}

impl EventSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EventSource::SystemPrompt => "system_prompt",
            EventSource::UserPrompt => "user_prompt",
            EventSource::AssistantOutput => "assistant_output",
            EventSource::ToolSchema => "tool_schema",
            EventSource::ToolCall => "tool_call",
            EventSource::ToolResult => "tool_result",
            EventSource::ShellCommand => "shell_command",
            EventSource::ShellOutput => "shell_output",
            EventSource::FileContent => "file_content",
            EventSource::GitOutput => "git_output",
            EventSource::TestOutput => "test_output",
            EventSource::LogOutput => "log_output",
            EventSource::McpMessage => "mcp_message",
            EventSource::Memory => "memory",
            EventSource::SubagentMessage => "subagent_message",
            EventSource::RetrievedCapsule => "retrieved_capsule",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDirection {
    /// Flowing towards the model.
    Inbound,
    /// Produced by the model.
    Outbound,
    /// Neither, e.g. a captured command.
    Internal,
}

/// Reference to the original bytes. Content-addressed, so identical content
/// is stored exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRef {
    /// `blake3:<hex>`
    pub hash: String,
    pub bytes: u64,
    /// Present once the blob has been persisted.
    pub stored: bool,
}

impl ContentRef {
    pub fn of(content: &str) -> Self {
        Self {
            hash: crate::hash_string(content),
            bytes: content.len() as u64,
            stored: false,
        }
    }
}

/// What a transformation did, in enough detail to explain and to replay it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformationRecord {
    /// Stable id of the transformer, e.g. `test.pytest`.
    pub transformer: String,
    pub version: u32,
    pub at_millis: u64,
    pub input_hash: String,
    pub output_hash: String,
    pub tokens_before: TokenCount,
    pub tokens_after: TokenCount,
    /// Invariants this transformer promised to preserve.
    pub invariants: Vec<Invariant>,
    /// Outcome of the quality firewall for this step.
    pub validation: ValidationOutcome,
    /// Populated when the firewall forced a less aggressive result.
    pub fallback: Option<FallbackReason>,
    /// Human readable notes shown by `ttk explain`.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    /// All validators passed.
    Passed,
    /// Validators passed but the transformer flagged reduced fidelity.
    PassedWithWarnings,
    /// A validator failed; the output was replaced by a safe fallback.
    Failed,
    /// No transformation was attempted (observe mode / unknown content).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum FallbackReason {
    /// A declared invariant disappeared from the output.
    InvariantLost(Vec<Invariant>),
    /// Structural re-parse of the output failed.
    ReparseFailed(String),
    /// The transformer produced more tokens than the input.
    NoGain,
    /// The transformer panicked or errored.
    TransformerError(String),
    /// Compression was disabled by configuration or mode.
    PolicyDisabled(String),
}

impl FallbackReason {
    pub fn summary(&self) -> String {
        match self {
            FallbackReason::InvariantLost(v) => {
                format!("{} invariant(s) would have been lost", v.len())
            }
            FallbackReason::ReparseFailed(e) => format!("re-parse failed: {e}"),
            FallbackReason::NoGain => "transformation did not reduce tokens".to_string(),
            FallbackReason::TransformerError(e) => format!("transformer error: {e}"),
            FallbackReason::PolicyDisabled(p) => format!("disabled by policy: {p}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEvent {
    pub schema_version: u32,
    pub id: EventId,
    pub session_id: SessionId,
    pub parent_id: Option<EventId>,
    /// Unix milliseconds. Stored as an integer so ordering is total and cheap.
    pub timestamp_millis: u64,

    pub source: EventSource,
    pub direction: EventDirection,
    pub content_type: ContentType,

    pub original_content_ref: ContentRef,
    /// `None` means "unchanged, use the original".
    pub transformed_content: Option<String>,

    pub source_hash: String,
    pub version: u32,

    pub trust_level: TrustLevel,
    pub sensitivity: SensitivityLevel,

    pub estimated_tokens_before: TokenCount,
    pub estimated_tokens_after: TokenCount,
    pub measured_tokens_before: Option<TokenCount>,
    pub measured_tokens_after: Option<TokenCount>,

    pub transformations: Vec<TransformationRecord>,
    pub invariants: Vec<Invariant>,
    pub capsule_id: Option<CapsuleId>,

    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl TokenEvent {
    /// Create an untransformed event for `content`.
    pub fn capture(
        session_id: SessionId,
        source: EventSource,
        direction: EventDirection,
        content_type: ContentType,
        content: &str,
    ) -> Self {
        let content_ref = ContentRef::of(content);
        let before = crate::tokens::estimate(content);
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            id: EventId::new(),
            session_id,
            parent_id: None,
            timestamp_millis: crate::ids::now_millis(),
            source,
            direction,
            content_type,
            source_hash: content_ref.hash.clone(),
            original_content_ref: content_ref,
            transformed_content: None,
            version: 1,
            trust_level: TrustLevel::ToolStructured,
            sensitivity: SensitivityLevel::Internal,
            estimated_tokens_before: before,
            estimated_tokens_after: before,
            measured_tokens_before: None,
            measured_tokens_after: None,
            transformations: Vec::new(),
            invariants: Vec::new(),
            capsule_id: None,
            metadata: Map::new(),
        }
    }

    pub fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.trust_level = trust;
        self
    }

    pub fn with_sensitivity(mut self, s: SensitivityLevel) -> Self {
        self.sensitivity = self.sensitivity.max(s);
        self
    }

    pub fn with_metadata(mut self, key: &str, value: Value) -> Self {
        self.metadata.insert(key.to_string(), value);
        self
    }

    /// Best available "before" count, preferring measured over estimated.
    pub fn tokens_before(&self) -> TokenCount {
        self.measured_tokens_before
            .unwrap_or(self.estimated_tokens_before)
    }

    pub fn tokens_after(&self) -> TokenCount {
        self.measured_tokens_after
            .unwrap_or(self.estimated_tokens_after)
    }

    pub fn tokens_saved(&self) -> TokenCount {
        self.tokens_before().saved_against(self.tokens_after())
    }

    /// Record a completed transformation and update the token accounting.
    pub fn apply(&mut self, output: String, record: TransformationRecord) {
        self.estimated_tokens_after = record.tokens_after;
        self.invariants = merge_invariants(&self.invariants, &record.invariants);
        self.transformations.push(record);
        self.transformed_content = Some(output);
    }

    /// The content that should actually be handed to the model.
    pub fn effective_content<'a>(&'a self, original: &'a str) -> &'a str {
        self.transformed_content.as_deref().unwrap_or(original)
    }

    pub fn was_transformed(&self) -> bool {
        self.transformed_content.is_some()
    }
}

fn merge_invariants(a: &[Invariant], b: &[Invariant]) -> Vec<Invariant> {
    let mut out: Vec<Invariant> = a.to_vec();
    for inv in b {
        if !out.contains(inv) {
            out.push(inv.clone());
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> TokenEvent {
        TokenEvent::capture(
            SessionId::new(),
            EventSource::ShellOutput,
            EventDirection::Inbound,
            ContentType::ShellOutput,
            "hello world\nsecond line\n",
        )
    }

    #[test]
    fn capture_sets_hash_and_counts() {
        let e = ev();
        assert!(e.source_hash.starts_with("blake3:"));
        assert_eq!(e.source_hash, e.original_content_ref.hash);
        assert!(e.estimated_tokens_before.value > 0);
        assert_eq!(e.tokens_saved().value, 0);
        assert!(!e.was_transformed());
    }

    #[test]
    fn apply_updates_accounting() {
        let mut e = ev();
        let before = e.estimated_tokens_before;
        let out = "hello".to_string();
        let after = crate::tokens::estimate(&out);
        e.apply(
            out.clone(),
            TransformationRecord {
                transformer: "test".into(),
                version: 1,
                at_millis: 0,
                input_hash: e.source_hash.clone(),
                output_hash: crate::hash_string(&out),
                tokens_before: before,
                tokens_after: after,
                invariants: vec![Invariant::custom("hello")],
                validation: ValidationOutcome::Passed,
                fallback: None,
                notes: vec![],
            },
        );
        assert!(e.was_transformed());
        assert_eq!(e.effective_content("ignored"), "hello");
        assert!(e.tokens_saved().value > 0);
        assert_eq!(e.invariants.len(), 1);
    }

    #[test]
    fn roundtrips_through_json() {
        let e = ev();
        let s = serde_json::to_string(&e).expect("serialize");
        let back: TokenEvent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(e, back);
    }
}
