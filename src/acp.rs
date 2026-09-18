//! ACP agent side types: what the agent sends us.

#[cfg(test)]
#[path = "acp_test.rs"]
mod acp_test;

use serde::Deserialize;
use serde_json::Value;

/// A message arriving on the agent's stdout: request, response, or notification.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Inbound {
    /// Agent → gateway request (e.g. `session/request_permission`,
    /// `fs/read_text_file`). Has an `id` and MUST be answered.
    Request {
        id: Id,
        method: String,
        params: Value,
    },
    /// Response to one of our requests (`initialize`, `session/new`,
    /// `session/prompt`).
    Response {
        id: Id,
        result: Option<Value>,
        error: Option<Error>,
    },
    /// One-way notification (e.g. `session/update`).
    Notification {
        method: String,
        params: Value,
    },
}

/// JSON-RPC id: number or string; notification-style absent ids normalize to Null.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
    Null,
}

impl Id {
    pub fn to_value(&self) -> Value {
        match self {
            Id::Num(n) => Value::from(*n),
            Id::Str(s) => Value::from(s.as_str()),
            Id::Null => Value::Null,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Error {
    pub code: i64,
    pub message: String,
}

/// Token accounting reported by the agent (`session/prompt` response `usage`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Usage {
    #[serde(rename = "totalTokens", default)]
    pub total_tokens: u64,
    #[serde(rename = "inputTokens", default)]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    pub output_tokens: u64,
    #[serde(rename = "thoughtTokens", default)]
    pub thought_tokens: u64,
    #[serde(rename = "cachedReadTokens", default)]
    pub cached_read_tokens: u64,
}

/// `session/update` notification payloads we care about.
#[derive(Debug, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum Update {
    /// Text chunk of the agent's answer.
    AgentMessageChunk {
        #[serde(default)]
        content: Content,
    },
    /// Text chunk of the agent's visible reasoning stream.
    AgentThoughtChunk {
        #[serde(default)]
        content: Content,
    },
    /// Final state of a tool call (arrives alongside others we ignore).
    /// Includes traecli's mid-turn `usage_update` snapshots — the OpenAI
    /// surface carries usage only on the final chunk, from the prompt
    /// response's authoritative numbers.
    #[serde(other)]
    Other,
}

/// ACP content blocks. Only text blocks carry payload for us.
#[derive(Debug, Deserialize, Default)]
pub struct Content {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub text: String,
}

/// `session/prompt` response: why the turn stopped.
#[derive(Debug, Deserialize)]
pub struct PromptResponse {
    #[serde(rename = "stopReason")]
    pub stop_reason: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Result of driving one full prompt turn.
#[derive(Debug)]
pub struct TurnOutcome {
    /// Concatenated agent text chunks.
    pub text: String,
    pub stop_reason: String,
    /// Token accounting from the prompt response, when the agent reports it.
    pub usage: Option<Usage>,
}
