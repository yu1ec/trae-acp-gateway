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

use crate::acp::Usage;
use crate::agent::TurnEvent;
use crate::http;
use crate::prompt::{PromptBlock, text_from_content};
use crate::AppState;

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
        self.thinking
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|t| t.as_str())
            == Some("enabled")
    }

    pub fn to_prompt_blocks(&self) -> Vec<PromptBlock> {
        let system_text = self
            .system
            .as_ref()
            .map(|s| match s {
                Value::String(t) => t.clone(),
                other => text_from_content(other),
            })
            .filter(|s| !s.is_empty());
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
            content.push(json!({ "type": "thinking", "thinking": t }));
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
    Json(req): Json<MessagesRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", "invalid_request_error")),
        ));
    }
    let _ = req.max_tokens;
    let blocks = req.to_prompt_blocks();
    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(error_response("no usable message content", "invalid_request_error")),
        ));
    }
    if req.stream {
        Ok(messages_stream(state, req, blocks).await.into_response())
    } else {
        messages_buffered(state, req, blocks).await
    }
}

use axum::response::IntoResponse;

async fn messages_buffered(
    state: AppState,
    req: MessagesRequest,
    blocks: Vec<PromptBlock>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let id = new_message_id();
    let model = req.model.clone();
    let thinking = req.thinking_enabled();
    let mut thought = String::new();

    match http::run_turn_with_callback(state.as_ref(), &blocks, &mut |ev| {
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
    req: MessagesRequest,
    blocks: Vec<PromptBlock>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let id = new_message_id();
    let model = req.model.clone();
    let thinking = req.thinking_enabled();
    let (tx, rx) = mpsc::channel(64);
    let state = state.clone();

    tokio::spawn(async move {
        let send = |ev: Event| tx.try_send(Ok(ev));
        let _ = send(Event::default().event("message_start").data(
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
        ));

        let mut block_index: u32 = 0;
        let mut thought_buf = String::new();
        let mut text_buf = String::new();

        if thinking {
            let _ = send(Event::default().event("content_block_start").data(
                json!({
                    "type": "content_block_start",
                    "index": block_index,
                    "content_block": { "type": "thinking", "thinking": "" }
                })
                .to_string(),
            ));
        }

        let result = http::run_turn_with_callback(state.as_ref(), &blocks, &mut |ev| {
            match ev {
                TurnEvent::Thought(t) if thinking => {
                    thought_buf.push_str(t);
                    let _ = send(Event::default().event("content_block_delta").data(
                        json!({
                            "type": "content_block_delta",
                            "index": block_index,
                            "delta": { "type": "thinking_delta", "thinking": t }
                        })
                        .to_string(),
                    ));
                }
                TurnEvent::Thought(_) => {}
                TurnEvent::Message(t) => {
                    if text_buf.is_empty() {
                        if thinking {
                            let _ = send(Event::default().event("content_block_stop").data(
                                json!({ "type": "content_block_stop", "index": block_index })
                                    .to_string(),
                            ));
                            block_index += 1;
                        }
                        let _ = send(Event::default().event("content_block_start").data(
                            json!({
                                "type": "content_block_start",
                                "index": block_index,
                                "content_block": { "type": "text", "text": "" }
                            })
                            .to_string(),
                        ));
                    }
                    text_buf.push_str(t);
                    let _ = send(Event::default().event("content_block_delta").data(
                        json!({
                            "type": "content_block_delta",
                            "index": block_index,
                            "delta": { "type": "text_delta", "text": t }
                        })
                        .to_string(),
                    ));
                }
            }
            Ok(())
        })
        .await;

        match result {
            Ok((_, outcome)) => {
                if thinking && text_buf.is_empty() && !thought_buf.is_empty() {
                    // only thinking, no text block started
                } else if !text_buf.is_empty() {
                    let _ = send(Event::default().event("content_block_stop").data(
                        json!({ "type": "content_block_stop", "index": block_index }).to_string(),
                    ));
                } else if thinking {
                    let _ = send(Event::default().event("content_block_stop").data(
                        json!({ "type": "content_block_stop", "index": block_index }).to_string(),
                    ));
                }
                let finish = stop_reason(&outcome.stop_reason);
                let usage = usage_json(outcome.usage.as_ref());
                let _ = send(Event::default().event("message_delta").data(
                    json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": finish, "stop_sequence": null },
                        "usage": usage,
                    })
                    .to_string(),
                ));
                let _ = send(Event::default().event("message_stop").data(
                    json!({ "type": "message_stop" }).to_string(),
                ));
            }
            Err(e) => {
                tracing::error!(error = %e, "anthropic stream turn failed");
                let _ = send(Event::default().event("error").data(
                    error_response(&e.to_string(), "api_error").to_string(),
                ));
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
