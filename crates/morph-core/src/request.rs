use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::message::{Message, ToolChoice, ToolDefinition};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningConfig {
    pub effort: ReasoningEffort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    /// "json_object", "json_schema", or "text".
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
}

/// Which wire protocol a request arrived as and left as — carried through the
/// whole pipeline so error responses, logs, and stats can be attributed
/// correctly, and so the response can be encoded back in the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetadata {
    pub request_id: String,
    pub ingress_protocol: String,
    #[serde(with = "humantime_serde_compat")]
    pub received_at: SystemTime,
}

/// The wire format independent request every `ProtocolAdapter` parses into
/// and every `ProviderAdapter` consumes. This is the seam that makes "any
/// client x any provider" possible: protocol adapters and provider adapters
/// never talk to each other directly, only through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub stop: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    pub metadata: RequestMetadata,
    /// Provider-specific fields that don't map to any canonical field, kept
    /// so protocol adapters can round-trip unusual client parameters instead
    /// of silently dropping them.
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    Error,
}

/// One event in a `ProviderAdapter`'s response stream. Every provider call —
/// streaming or not — is modeled as a stream of these; a non-streaming
/// provider adapter simply emits one `MessageStart`/.../`MessageStop` burst
/// synchronously. This lets `ProtocolAdapter` decide independently whether to
/// forward events as SSE or buffer them into one JSON body, based on what the
/// *client* asked for rather than what the *provider* natively does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseEvent {
    MessageStart {
        id: String,
        model: String,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        partial_json: String,
    },
    ToolCallEnd {
        index: usize,
    },
    Usage(Usage),
    MessageStop {
        stop_reason: StopReason,
    },
}

/// A fully materialized (non-streaming) response, produced by collapsing a
/// `ResponseEvent` stream. Kept separate from the event stream itself so
/// renderers/middleware that need "the whole answer" don't have to
/// re-implement stream aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<crate::message::ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

mod humantime_serde_compat {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let millis = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        millis.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let millis = u64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_millis(millis))
    }
}
