//! OpenAI Responses API surface.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::acp::Usage;
use crate::agent::TurnEvent;
use crate::config::Config;
use crate::http;
use crate::openai::{error_response, usage_json};
use crate::prompt::{PromptBlock, blocks_from_content, text_from_content};

#[derive(Clone, Default)]
pub struct ResponseStore {
    inner: Arc<Mutex<HashMap<String, StoredResponse>>>,
}

#[derive(Clone, Debug)]
struct StoredResponse {
    body: Value,
    transcript: String,
}

impl ResponseStore {
    fn get(&self, id: &str) -> Option<StoredResponse> {
        self.inner.lock().ok()?.get(id).cloned()
    }

    pub fn insert(&self, id: String, body: Value, transcript: String) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(id, StoredResponse { body, transcript });
        }
    }
}

#[derive(Clone)]
pub struct GatewayState {
    pub config: Arc<Config>,
    pub responses: ResponseStore,
}

pub type SharedState = Arc<GatewayState>;

#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub previous_response_id: Option<String>,
}

impl ResponsesRequest {
    pub fn to_prompt_blocks(&self, store: &ResponseStore) -> Vec<PromptBlock> {
        let mut blocks = Vec::new();
        if let Some(prev) = &self.previous_response_id {
            if let Some(stored) = store.get(prev) {
                blocks.push(PromptBlock::text(format!(
                    "Previous conversation:\n{}\n\n",
                    stored.transcript
                )));
            }
        }
        if let Some(instr) = &self.instructions {
            if !instr.is_empty() {
                blocks.push(PromptBlock::text(format!("{instr}\n")));
            }
        }
        match &self.input {
            Value::String(s) if !s.is_empty() => blocks.push(PromptBlock::text(s.clone())),
            Value::Array(items) => {
                for item in items {
                    blocks.extend(blocks_from_response_item(item));
                }
            }
            _ => {}
        }
        blocks
    }

    pub fn input_summary(&self) -> String {
        match &self.input {
            Value::String(s) => s.clone(),
            Value::Array(items) => items
                .iter()
                .map(|item| match item.get("type").and_then(|v| v.as_str()) {
                    Some("message") => {
                        let c = item.get("content").cloned().unwrap_or(Value::Null);
                        text_from_content(&c)
                    }
                    Some("input_text") => item
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => String::new(),
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

fn blocks_from_response_item(item: &Value) -> Vec<PromptBlock> {
    let Some(t) = item.get("type").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    match t {
        "message" => {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = item.get("content").cloned().unwrap_or(Value::Null);
            let prefix = if role == "assistant" {
                "[assistant]: "
            } else {
                "[user]: "
            };
            let mut out = vec![PromptBlock::text(prefix.to_string())];
            out.extend(blocks_from_content(&content));
            out.push(PromptBlock::text("\n".to_string()));
            out
        }
        "input_text" => item
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| vec![PromptBlock::text(s.to_string())])
            .unwrap_or_default(),
        "input_image" => blocks_from_content(&Value::Array(vec![item.clone()])),
        _ => Vec::new(),
    }
}

fn new_response_id() -> String {
    format!(
        "resp_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn build_response(id: &str, model: &str, text: &str, usage: Option<&Usage>) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": unix_now(),
        "status": "completed",
        "model": model,
        "output": [{
            "type": "message",
            "id": format!("msg_{id}"),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        }],
        "usage": usage_json(usage),
    })
}

pub async fn create_response(
    State(state): State<SharedState>,
    Json(req): Json<ResponsesRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let blocks = req.to_prompt_blocks(&state.responses);
    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(error_response("input must not be empty", 400)),
        ));
    }
    if req.stream {
        Ok(responses_stream(state, req, blocks).await.into_response())
    } else {
        responses_buffered(state, req, blocks).await
    }
}

async fn responses_buffered(
    state: SharedState,
    req: ResponsesRequest,
    blocks: Vec<PromptBlock>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let id = new_response_id();
    let model = req.model.clone();
    let input_summary = req.input_summary();
    let prev = req
        .previous_response_id
        .as_ref()
        .and_then(|p| state.responses.get(p))
        .map(|s| s.transcript.clone())
        .unwrap_or_default();

    match http::run_turn(state.as_ref(), &blocks).await {
        Ok((_, outcome)) => {
            let body = build_response(&id, &model, &outcome.text, outcome.usage.as_ref());
            let transcript = format!(
                "{prev}{}User: {input_summary}\nAssistant: {}\n",
                if prev.is_empty() { "" } else { "" },
                outcome.text
            );
            state.responses.insert(id.clone(), body.clone(), transcript);
            Ok(Json(body).into_response())
        }
        Err(e) => {
            tracing::error!(error = %e, "responses turn failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response(&e.to_string(), 500)),
            ))
        }
    }
}

async fn responses_stream(
    state: SharedState,
    req: ResponsesRequest,
    blocks: Vec<PromptBlock>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let id = new_response_id();
    let model = req.model.clone();
    let input_summary = req.input_summary();
    let prev = req
        .previous_response_id
        .as_ref()
        .and_then(|p| state.responses.get(p))
        .map(|s| s.transcript.clone())
        .unwrap_or_default();
    let store = state.responses.clone();
    let cfg = state.clone();
    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        let send = |ev: Event| tx.try_send(Ok(ev));
        let _ = send(
            Event::default().event("response.created").data(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": id,
                        "object": "response",
                        "status": "in_progress",
                        "model": model,
                    }
                })
                .to_string(),
            ),
        );
        let output_item_id = format!("msg_{id}");
        let _ = send(
            Event::default().event("response.output_item.added").data(
                json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": output_item_id,
                        "role": "assistant",
                        "status": "in_progress",
                        "content": [],
                    }
                })
                .to_string(),
            ),
        );
        let _ = send(
            Event::default().event("response.content_part.added").data(
                json!({
                    "type": "response.content_part.added",
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": "" },
                })
                .to_string(),
            ),
        );

        let mut text_buf = String::new();
        let result = http::run_turn_with_callback(&cfg, &blocks, &mut |ev| {
            if let TurnEvent::Message(t) = ev {
                text_buf.push_str(t);
                let _ = send(
                    Event::default().event("response.output_text.delta").data(
                        json!({
                            "type": "response.output_text.delta",
                            "output_index": 0,
                            "content_index": 0,
                            "delta": t,
                        })
                        .to_string(),
                    ),
                );
            }
            Ok(())
        })
        .await;

        match result {
            Ok((_, outcome)) => {
                let text = if text_buf.is_empty() {
                    outcome.text.clone()
                } else {
                    text_buf
                };
                let _ = send(
                    Event::default().event("response.output_text.done").data(
                        json!({
                            "type": "response.output_text.done",
                            "output_index": 0,
                            "content_index": 0,
                            "text": text,
                        })
                        .to_string(),
                    ),
                );
                let body = build_response(&id, &model, &text, outcome.usage.as_ref());
                let transcript = format!("{prev}User: {input_summary}\nAssistant: {text}\n");
                store.insert(id.clone(), body.clone(), transcript);
                let _ =
                    send(Event::default().event("response.completed").data(
                        json!({ "type": "response.completed", "response": body }).to_string(),
                    ));
            }
            Err(e) => {
                tracing::error!(error = %e, "responses stream failed");
                let _ = send(
                    Event::default()
                        .event("error")
                        .data(error_response(&e.to_string(), 500).to_string()),
                );
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

#[cfg(test)]
#[path = "openai_responses_test.rs"]
mod openai_responses_test;

pub async fn get_response(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.responses.get(&id) {
        Some(stored) => Ok(Json(stored.body)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(error_response("response not found", 404)),
        )),
    }
}
