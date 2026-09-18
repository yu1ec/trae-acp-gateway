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

pub fn chat_response(id: &str, text: &str, finish: &str, usage: Option<&crate::acp::Usage>) -> Value {
    let mut resp = json!({
        "id": id,
        "object": "chat.completion",
        "created": unix_now(),
        "model": MODEL_NAME,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": finish,
        }],
        "usage": usage_json(usage),
    });
    resp["choices"][0]["message"]["reasoning_content"] = Value::Null;
    resp
}

/// OpenAI `usage` object. Without agent numbers we report the shape with
/// zeros — a missing/null `usage` makes streaming clients (e.g. omp) show a
/// `-1%` context instead of an honest value.
pub fn usage_json(usage: Option<&crate::acp::Usage>) -> Value {
    let u = usage.cloned().unwrap_or_default();
    json!({
        "prompt_tokens": u.input_tokens,
        "completion_tokens": u.output_tokens,
        "total_tokens": u.total_tokens,
        "prompt_tokens_details": {
            "cached_tokens": u.cached_read_tokens,
            "reasoning_tokens": u.thought_tokens,
        },
        "completion_tokens_details": {
            "reasoning_tokens": u.thought_tokens,
        },
    })
}

/// One SSE chunk of a streamed chat completion. `usage` attaches only to the
/// final chunk (OpenAI `stream_options.include_usage` shape); omp reads it to
/// compute context-window percentage.
pub fn chat_chunk(id: &str, delta: Value, finish: Option<&str>, usage: Option<&crate::acp::Usage>) -> Value {
    let mut chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": unix_now(),
        "model": MODEL_NAME,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish,
        }],
    });
    if let Some(u) = usage {
        chunk["usage"] = usage_json(Some(u));
    }
    chunk
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
