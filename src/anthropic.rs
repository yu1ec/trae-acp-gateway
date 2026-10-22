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
use tokio_stream::wrappers::ReceiverStream;

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
        _ => "end_turn",
    }
}

fn usage_json(usage: Option<&Usage>) -> Value {
    let u = usage.cloned().unwrap_or_default();
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
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
fn thinking_signature(id: &str) -> String {
    format!("sig_{id}")
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
                "signature": thinking_signature(id),
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
    let (tx, rx) = mpsc::channel(64);
    let state = state.clone();

    tokio::spawn(async move {
        // A failed try_send means the client is gone (receiver dropped) or is
        // too slow; abort the turn so the spawned agent is killed instead of
        // leaking.
        let send = |ev: Event| -> anyhow::Result<()> {
            match tx.try_send(Ok(ev)) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    anyhow::bail!("SSE buffer full; aborting turn")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    anyhow::bail!("client disconnected mid-turn")
                }
            }
        };
        if send(
            Event::default().event("message_start").data(
                json!({
                    "type": "message_start",
                    "message": {
                        "id": id,
                        "type": "message",
                        "role": "assistant",
                        "model": model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 },
                    }
                })
                .to_string(),
            ),
        )
        .is_err()
        {
            return;
        }

        let mut block_index: u32 = 0;
        let mut thinking_open = false;
        let mut text_open = false;

        let result = http::run_turn_with_callback(state.as_ref(), &workdir, &blocks, &mut |ev| {
            match ev {
                TurnEvent::Thought(t) if thinking => {
                    // 思考块必须在正文之前；正文已开始后到达的思考丢弃，
                    // 避免发出 Claude Code 无法解析的块顺序。
                    if text_open {
                        return Ok(());
                    }
                    if !thinking_open {
                        send(
                            Event::default().event("content_block_start").data(
                                json!({
                                    "type": "content_block_start",
                                    "index": block_index,
                                    "content_block": {
                                        "type": "thinking",
                                        "thinking": "",
                                        "signature": ""
                                    }
                                })
                                .to_string(),
                            ),
                        )?;
                        thinking_open = true;
                    }
                    send(
                        Event::default().event("content_block_delta").data(
                            json!({
                                "type": "content_block_delta",
                                "index": block_index,
                                "delta": { "type": "thinking_delta", "thinking": t }
                            })
                            .to_string(),
                        ),
                    )?;
                }
                TurnEvent::Thought(_) => {}
                TurnEvent::Message(t) => {
                    if thinking_open {
                        send(
                            Event::default().event("content_block_delta").data(
                                json!({
                                    "type": "content_block_delta",
                                    "index": block_index,
                                    "delta": {
                                        "type": "signature_delta",
                                        "signature": thinking_signature(&id)
                                    }
                                })
                                .to_string(),
                            ),
                        )?;
                        send(
                            Event::default().event("content_block_stop").data(
                                json!({ "type": "content_block_stop", "index": block_index })
                                    .to_string(),
                            ),
                        )?;
                        thinking_open = false;
                        block_index += 1;
                    }
                    if !text_open {
                        send(
                            Event::default().event("content_block_start").data(
                                json!({
                                    "type": "content_block_start",
                                    "index": block_index,
                                    "content_block": { "type": "text", "text": "" }
                                })
                                .to_string(),
                            ),
                        )?;
                        text_open = true;
                    }
                    send(
                        Event::default().event("content_block_delta").data(
                            json!({
                                "type": "content_block_delta",
                                "index": block_index,
                                "delta": { "type": "text_delta", "text": t }
                            })
                            .to_string(),
                        ),
                    )?;
                }
            }
            Ok(())
        })
        .await;

        match result {
            Ok((_, outcome)) => {
                if thinking_open {
                    let _ = send(
                        Event::default().event("content_block_delta").data(
                            json!({
                                "type": "content_block_delta",
                                "index": block_index,
                                "delta": {
                                    "type": "signature_delta",
                                    "signature": thinking_signature(&id)
                                }
                            })
                            .to_string(),
                        ),
                    );
                    let _ = send(Event::default().event("content_block_stop").data(
                        json!({ "type": "content_block_stop", "index": block_index }).to_string(),
                    ));
                } else if text_open {
                    let _ = send(Event::default().event("content_block_stop").data(
                        json!({ "type": "content_block_stop", "index": block_index }).to_string(),
                    ));
                }
                let finish = stop_reason(&outcome.stop_reason);
                let usage = usage_json(outcome.usage.as_ref());
                let _ = send(
                    Event::default().event("message_delta").data(
                        json!({
                            "type": "message_delta",
                            "delta": { "stop_reason": finish, "stop_sequence": null },
                            "usage": usage,
                        })
                        .to_string(),
                    ),
                );
                let _ = send(
                    Event::default()
                        .event("message_stop")
                        .data(json!({ "type": "message_stop" }).to_string()),
                );
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

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
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
}
