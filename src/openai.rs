//! OpenAI-compatible HTTP surface.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};

pub const MODEL_NAME: &str = "trae";

/// OpenAI chat request. We keep only what we consume; `model` is accepted but
/// ignored (trae decides its own model), `stream` switches the response mode.
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    // Unrecognized fields (temperature, tools, ...) are ignored by serde.
}

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// String for plain text; array of parts for multimodal callers.
    #[serde(default)]
    pub content: Value,
}

impl ChatMessage {
    /// Extract plain text from string or content-parts-array forms.
    fn text(&self) -> String {
        match &self.content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    }
}

/// Flatten the OpenAI message list into a single prompt string.
/// `system` messages go first verbatim; the rest are role-prefixed and
/// newline-joined. ACP turns are single-shot here (session per request).
pub fn flatten_messages(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    let mut body = Vec::new();
    for m in messages {
        let text = m.text();
        match m.role.as_str() {
            "system" | "developer" => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text);
            }
            "assistant" => body.push(format!("[assistant]: {text}")),
            // user and anything else
            _ => body.push(format!("[user]: {text}")),
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&body.join("\n"));
    out
}

/// Map ACP stop reasons to OpenAI finish_reason.
pub fn finish_reason(stop_reason: &str) -> &'static str {
    match stop_reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        // No OpenAI equivalent for agent-loop limits / refusals.
        "max_turn_requests" | "refusal" => "stop",
        "cancelled" => "stop",
        _ => "stop",
    }
}

pub fn models_response(models: &[crate::agent::ModelInfo]) -> Value {
    let data: Vec<Value> = models
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "object": "model",
                "created": unix_now(),
                "owned_by": "trae-acp-gateway",
            })
        })
        .collect();
    json!({ "object": "list", "data": data })
}

pub fn chat_response(id: &str, text: &str, finish: &str) -> Value {
    json!({
        "id": id,
        "object": "chat.completion",
        "created": unix_now(),
        "model": MODEL_NAME,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": finish,
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
}

/// One SSE chunk of a streamed chat completion.
pub fn chat_chunk(id: &str, delta: Value, finish: Option<&str>) -> Value {
    json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": unix_now(),
        "model": MODEL_NAME,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish,
        }],
    })
}

pub fn error_response(message: &str, code: u16) -> Value {
    json!({
        "error": { "message": message, "type": "gateway_error", "code": code },
    })
}

pub fn new_completion_id() -> String {
    format!("chatcmpl-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
