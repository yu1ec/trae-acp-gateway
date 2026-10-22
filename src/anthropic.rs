//! Anthropic Messages API surface.

use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::AppState;
use crate::acp::Usage;
use crate::agent::TurnEvent;
use crate::http;
use crate::prompt::{PromptBlock, text_from_content};

#[derive(Debug, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub thinking: Option<Value>,
}

fn default_max_tokens() -> u64 {
    4096
}

#[derive(Debug, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
}

impl MessagesRequest {
    pub fn thinking_enabled(&self) -> bool {
        // Claude Code 2.x sends `{"type":"adaptive","display":"updates"}` for
        // models that reason; older clients send `{"type":"enabled",...}`.
        self.thinking
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|t| t.as_str())
            .is_some_and(|t| matches!(t, "enabled" | "adaptive"))
    }

    /// Flattened client system prompt (string or content blocks).
    pub fn system_text(&self) -> Option<String> {
        self.system
            .as_ref()
            .map(|s| match s {
                Value::String(t) => t.clone(),
                other => text_from_content(other),
            })
            .filter(|s| !s.is_empty())
    }

    /// The harness-injected environment block (the only place Claude Code
    /// reports its cwd). Scans user turns newest-first so a later `cd` wins.
    pub fn environment_context_text(&self) -> Option<String> {
        const MARKER: &str = "You have been invoked in the following environment";
        self.messages.iter().rev().find_map(|m| {
            let text = text_from_content(&m.content);
            text.contains(MARKER).then_some(text)
        })
    }

    pub fn to_prompt_blocks(&self) -> Vec<PromptBlock> {
        let system_text = self.system_text();
        crate::prompt::flatten_role_messages(
            system_text.as_deref(),
            &self
                .messages
                .iter()
                .map(|m| (m.role.clone(), &m.content))
                .collect::<Vec<_>>(),
        )
    }
}

pub fn error_response(message: &str, status_type: &str) -> Value {
    json!({
        "type": "error",
        "error": { "type": status_type, "message": message },
    })
}

fn stop_reason(acp: &str) -> &'static str {
    match acp {
        "max_tokens" => "max_tokens",
        "refusal" => "refusal",
        "end_turn" => "end_turn",
        // Anthropic has no equivalent for these ACP reasons. `cancelled` and
        // `max_turn_requests` both mean the turn ended prematurely, so surface
        // them in the logs instead of silently reporting a clean end_turn.
        "cancelled" => {
            tracing::warn!(
                stop_reason = acp,
                "agent turn was cancelled; reporting end_turn"
            );
            "end_turn"
        }
        "max_turn_requests" => {
            tracing::warn!(
                stop_reason = acp,
                "agent hit its max turn request limit; reporting end_turn"
            );
            "end_turn"
        }
        other => {
            tracing::warn!(
                stop_reason = other,
                "unknown ACP stop reason; reporting end_turn"
            );
            "end_turn"
        }
    }
}

fn usage_json(usage: Option<&Usage>) -> Value {
    let u = usage.cloned().unwrap_or_default();
    // Trae's `inputTokens` already includes cached reads, whereas Anthropic's
    // `input_tokens` excludes them. Split them out so clients (Claude Code's
    // context/cost display) don't count the cache twice.
    let cached = u.cached_read_tokens.min(u.input_tokens);
    json!({
        "input_tokens": u.input_tokens.saturating_sub(cached),
        "output_tokens": u.output_tokens,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": cached,
    })
}

fn new_message_id() -> String {
    format!(
        "msg_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

/// Claude Code expects every thinking block to carry a non-empty signature,
/// otherwise it cannot round-trip the block. The upstream turn does not produce
/// one, so hand out a stable placeholder.
fn thinking_signature(id: &str, index: u32) -> String {
    format!("sig_{id}_{index}")
}

fn content_block_start(index: u32, block: Value) -> Value {
    json!({ "type": "content_block_start", "index": index, "content_block": block })
}

fn content_block_delta(index: u32, delta: Value) -> Value {
    json!({ "type": "content_block_delta", "index": index, "delta": delta })
}

fn content_block_stop(index: u32) -> Value {
    json!({ "type": "content_block_stop", "index": index })
}

fn sse_event(payload: Value) -> Event {
    let name = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Event::default().event(name).data(payload.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    None,
    Thinking,
    Text,
}

/// Tracks the content block currently open on the SSE stream.
///
/// Thought and text chunks interleave freely during a multi-step turn. Claude
/// Code requests the interleaved-thinking beta, so a thought arriving after
/// text closes the text block and opens a new thinking block instead of being
/// dropped. Each transition closes the previous block and bumps the index.
struct BlockWriter {
    index: u32,
    open: OpenBlock,
}

impl BlockWriter {
    fn new() -> Self {
        Self {
            index: 0,
            open: OpenBlock::None,
        }
    }

    /// Close the open block, if any, returning its trailing events.
    fn close(&mut self, message_id: &str) -> Vec<Value> {
        let mut out = Vec::new();
        match self.open {
            OpenBlock::None => return out,
            OpenBlock::Thinking => out.push(content_block_delta(
                self.index,
                json!({
                    "type": "signature_delta",
                    "signature": thinking_signature(message_id, self.index),
                }),
            )),
            OpenBlock::Text => {}
        }
        out.push(content_block_stop(self.index));
        self.index += 1;
        self.open = OpenBlock::None;
        out
    }

    fn push_thinking(&mut self, text: &str, message_id: &str) -> Vec<Value> {
        let mut out = Vec::new();
        if self.open != OpenBlock::Thinking {
            out.extend(self.close(message_id));
            out.push(content_block_start(
                self.index,
                json!({ "type": "thinking", "thinking": "", "signature": "" }),
            ));
            self.open = OpenBlock::Thinking;
        }
        out.push(content_block_delta(
            self.index,
            json!({ "type": "thinking_delta", "thinking": text }),
        ));
        out
    }

    fn push_text(&mut self, text: &str, message_id: &str) -> Vec<Value> {
        let mut out = Vec::new();
        if self.open != OpenBlock::Text {
            out.extend(self.close(message_id));
            out.push(content_block_start(
                self.index,
                json!({ "type": "text", "text": "" }),
            ));
            self.open = OpenBlock::Text;
        }
        out.push(content_block_delta(
            self.index,
            json!({ "type": "text_delta", "text": text }),
        ));
        out
    }
}

pub fn message_response(
    id: &str,
    model: &str,
    text: &str,
    thought: Option<&str>,
    thinking_enabled: bool,
    finish: &str,
    usage: Option<&Usage>,
) -> Value {
    let mut content = Vec::new();
    if thinking_enabled {
        if let Some(t) = thought.filter(|s| !s.is_empty()) {
            content.push(json!({
                "type": "thinking",
                "thinking": t,
                "signature": thinking_signature(id, 0),
            }));
        }
    }
    content.push(json!({ "type": "text", "text": text }));
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": finish,
        "stop_sequence": null,
        "usage": usage_json(usage),
    })
}

pub async fn messages(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MessagesRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(error_response(
                "messages must not be empty",
                "invalid_request_error",
            )),
        ));
    }
    let _ = req.max_tokens;
    let system_text = req.system_text();
    let environment_context = req.environment_context_text();
    let workdir = http::resolve_workdir(
        &state,
        &headers,
        None,
        environment_context.as_deref(),
        system_text.as_deref(),
    )
    .map_err(|e| {
        tracing::warn!(error = %e, "invalid workdir");
        (
            StatusCode::BAD_REQUEST,
            Json(error_response(
                &format!("invalid workdir: {e}"),
                "invalid_request_error",
            )),
        )
    })?;
    let blocks = req.to_prompt_blocks();
    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(error_response(
                "no usable message content",
                "invalid_request_error",
            )),
        ));
    }
    if req.stream {
        Ok(messages_stream(state, workdir, req, blocks)
            .await
            .into_response())
    } else {
        messages_buffered(state, workdir, req, blocks).await
    }
}

use axum::response::IntoResponse;

async fn messages_buffered(
    state: AppState,
    workdir: String,
    req: MessagesRequest,
    blocks: Vec<PromptBlock>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let id = new_message_id();
    let model = req.model.clone();
    let thinking = req.thinking_enabled();
    let mut thought = String::new();

    match http::run_turn_with_callback(state.as_ref(), &workdir, &blocks, &mut |ev| {
        if let TurnEvent::Thought(t) = ev {
            thought.push_str(t);
        }
        Ok(())
    })
    .await
    {
        Ok((_, outcome)) => {
            let body = message_response(
                &id,
                &model,
                &outcome.text,
                if thinking { Some(&thought) } else { None },
                thinking,
                stop_reason(&outcome.stop_reason),
                outcome.usage.as_ref(),
            );
            Ok(Json(body).into_response())
        }
        Err(e) => {
            tracing::error!(error = %e, "anthropic turn failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response(&e.to_string(), "api_error")),
            ))
        }
    }
}

async fn messages_stream(
    state: AppState,
    workdir: String,
    req: MessagesRequest,
    blocks: Vec<PromptBlock>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let id = new_message_id();
    let model = req.model.clone();
    let thinking = req.thinking_enabled();
    // Unbounded on purpose: the turn callback is synchronous and cannot await
    // capacity. A bounded queue + try_send used to abort the whole turn
    // (killing the agent) whenever the client was briefly slow; one turn's
    // worth of SSE events is small, so buffering is the better trade.
    let (tx, rx) = mpsc::unbounded_channel();
    let state = state.clone();

    tokio::spawn(async move {
        // A failed send means the receiver was dropped (client hung up), so
        // abort the turn and let `kill_on_drop` clean up the agent.
        let send = |ev: Event| -> anyhow::Result<()> {
            tx.send(Ok(ev))
                .map_err(|_| anyhow::anyhow!("client disconnected mid-turn"))
        };
        if send(sse_event(json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                },
            }
        })))
        .is_err()
        {
            return;
        }

        let mut blocks_out = BlockWriter::new();

        let result = http::run_turn_with_callback(state.as_ref(), &workdir, &blocks, &mut |ev| {
            let payloads = match ev {
                TurnEvent::Thought(t) if thinking => blocks_out.push_thinking(t, &id),
                TurnEvent::Thought(_) => Vec::new(),
                TurnEvent::Message(t) => blocks_out.push_text(t, &id),
            };
            for payload in payloads {
                send(sse_event(payload))?;
            }
            Ok(())
        })
        .await;

        match result {
            Ok((_, outcome)) => {
                for payload in blocks_out.close(&id) {
                    let _ = send(sse_event(payload));
                }
                let finish = stop_reason(&outcome.stop_reason);
                let usage = usage_json(outcome.usage.as_ref());
                let _ = send(sse_event(json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": finish, "stop_sequence": null },
                    "usage": usage,
                })));
                let _ = send(sse_event(json!({ "type": "message_stop" })));
            }
            Err(e) => {
                if tx.is_closed() {
                    // Client hung up: the turn was already aborted and the
                    // agent killed, so there is nobody left to send to.
                    tracing::debug!(error = %e, "anthropic client disconnected; turn aborted");
                } else {
                    tracing::error!(error = %e, "anthropic stream turn failed");
                    let _ = send(
                        Event::default()
                            .event("error")
                            .data(error_response(&e.to_string(), "api_error").to_string()),
                    );
                }
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_claude_code_adaptive_thinking() {
        let req: MessagesRequest = serde_json::from_value(json!({
            "model": "claude",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }],
            "thinking": { "type": "adaptive", "display": "updates" }
        }))
        .unwrap();
        assert!(req.thinking_enabled());
    }

    #[test]
    fn accepts_legacy_enabled_thinking() {
        let req: MessagesRequest = serde_json::from_value(json!({
            "model": "claude",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }],
            "thinking": { "type": "enabled", "budget_tokens": 4096 }
        }))
        .unwrap();
        assert!(req.thinking_enabled());
    }

    #[test]
    fn rejects_disabled_or_missing_thinking() {
        for thinking in [json!(null), json!({ "type": "disabled" })] {
            let req: MessagesRequest = serde_json::from_value(json!({
                "model": "claude",
                "max_tokens": 1024,
                "messages": [{ "role": "user", "content": "hi" }],
                "thinking": thinking
            }))
            .unwrap();
            assert!(!req.thinking_enabled());
        }
    }

    #[test]
    fn extracts_latest_environment_context() {
        let req: MessagesRequest = serde_json::from_value(json!({
            "model": "claude",
            "max_tokens": 1024,
            "messages": [
                { "role": "user", "content": "# Environment\nYou have been invoked in the following environment: \n - Primary working directory: /old" },
                { "role": "assistant", "content": "ok" },
                { "role": "user", "content": [{ "type": "text", "text": "# Environment\nYou have been invoked in the following environment: \n - Primary working directory: /new" }] }
            ]
        }))
        .unwrap();
        let context = req.environment_context_text().unwrap();
        assert!(context.contains("/new"), "{context}");
        assert!(!context.contains("/old"), "{context}");
    }

    #[test]
    fn flattens_user_text_message() {
        let req: MessagesRequest = serde_json::from_value(json!({
            "model": "claude",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .unwrap();
        let blocks = req.to_prompt_blocks();
        assert!(!blocks.is_empty());
    }

    #[test]
    fn maps_stop_reasons_to_anthropic_values() {
        assert_eq!(stop_reason("max_tokens"), "max_tokens");
        assert_eq!(stop_reason("refusal"), "refusal");
        assert_eq!(stop_reason("end_turn"), "end_turn");
        assert_eq!(stop_reason("cancelled"), "end_turn");
        assert_eq!(stop_reason("max_turn_requests"), "end_turn");
        assert_eq!(stop_reason("something_new"), "end_turn");
    }

    #[test]
    fn usage_splits_cached_tokens_out_of_input() {
        let usage = Usage {
            total_tokens: 21_372,
            input_tokens: 21_330,
            output_tokens: 42,
            thought_tokens: 10,
            cached_read_tokens: 7,
        };
        let usage = usage_json(Some(&usage));
        assert_eq!(usage["input_tokens"], 21_323);
        assert_eq!(usage["output_tokens"], 42);
        assert_eq!(usage["cache_read_input_tokens"], 7);
        assert_eq!(usage["cache_creation_input_tokens"], 0);
    }

    #[test]
    fn usage_without_agent_numbers_is_zeroed() {
        let usage = usage_json(None);
        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 0);
        assert_eq!(usage["cache_creation_input_tokens"], 0);
    }

    #[test]
    fn interleaved_thoughts_open_new_blocks() {
        let mut writer = BlockWriter::new();
        let mut events = Vec::new();
        events.extend(writer.push_thinking("a", "msg_1"));
        events.extend(writer.push_text("b", "msg_1"));
        events.extend(writer.push_thinking("c", "msg_1"));
        events.extend(writer.push_text("d", "msg_1"));
        events.extend(writer.close("msg_1"));

        let shape: Vec<(&str, u64)> = events
            .iter()
            .map(|e| {
                (
                    e["type"].as_str().unwrap(),
                    e["index"].as_u64().unwrap_or(0),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("content_block_start", 0),
                ("content_block_delta", 0), // thinking_delta
                ("content_block_delta", 0), // signature_delta
                ("content_block_stop", 0),
                ("content_block_start", 1),
                ("content_block_delta", 1), // text_delta
                ("content_block_stop", 1),
                ("content_block_start", 2),
                ("content_block_delta", 2), // thinking_delta
                ("content_block_delta", 2), // signature_delta
                ("content_block_stop", 2),
                ("content_block_start", 3),
                ("content_block_delta", 3), // text_delta
                ("content_block_stop", 3),
            ]
        );

        let deltas: Vec<&str> = events
            .iter()
            .filter(|e| e["type"] == "content_block_delta")
            .map(|e| e["delta"]["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            deltas,
            [
                "thinking_delta",
                "signature_delta",
                "text_delta",
                "thinking_delta",
                "signature_delta",
                "text_delta"
            ]
        );
    }

    #[test]
    fn close_is_a_noop_when_nothing_is_open() {
        let mut writer = BlockWriter::new();
        assert!(writer.close("msg_1").is_empty());
    }
}
