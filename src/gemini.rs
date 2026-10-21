//! Google Gemini generateContent API (v1beta-compatible paths).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::acp::Usage;
use crate::agent::TurnEvent;
use crate::http;
use crate::prompt::{PromptBlock, blocks_from_gemini_parts};

#[derive(Debug, Deserialize)]
pub struct GenerateContentRequest {
    #[serde(default)]
    pub contents: Vec<GeminiContent>,
    #[serde(default, rename = "systemInstruction")]
    pub system_instruction: Option<GeminiContent>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiContent {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub parts: Vec<Value>,
}

impl GenerateContentRequest {
    pub fn to_prompt_blocks(&self) -> Vec<PromptBlock> {
        let mut blocks = Vec::new();
        if let Some(sys) = &self.system_instruction {
            let sys_blocks = blocks_from_gemini_parts(&sys.parts);
            if !sys_blocks.is_empty() {
                blocks.push(PromptBlock::text("System:\n".to_string()));
                blocks.extend(sys_blocks);
                blocks.push(PromptBlock::text("\n".to_string()));
            }
        }
        for c in &self.contents {
            let role = if c.role.is_empty() {
                "user"
            } else {
                c.role.as_str()
            };
            let prefix = if role == "model" {
                "[assistant]: "
            } else {
                "[user]: "
            };
            blocks.push(PromptBlock::text(prefix.to_string()));
            blocks.extend(blocks_from_gemini_parts(&c.parts));
            blocks.push(PromptBlock::text("\n".to_string()));
        }
        blocks
    }
}

pub fn gemini_error(message: &str, code: i32) -> Value {
    json!({
        "error": {
            "code": code,
            "message": message,
            "status": "INVALID_ARGUMENT",
        }
    })
}

fn finish_reason(acp: &str) -> &'static str {
    match acp {
        "max_tokens" => "MAX_TOKENS",
        _ => "STOP",
    }
}

fn usage_metadata(usage: Option<&Usage>) -> Value {
    let u = usage.cloned().unwrap_or_default();
    json!({
        "promptTokenCount": u.input_tokens,
        "candidatesTokenCount": u.output_tokens,
        "totalTokenCount": u.total_tokens,
    })
}

pub fn generate_response(
    text: &str,
    thought: Option<&str>,
    finish: &str,
    usage: Option<&Usage>,
) -> Value {
    let mut parts = Vec::new();
    if let Some(t) = thought.filter(|s| !s.is_empty()) {
        parts.push(json!({ "text": t, "thought": true }));
    }
    parts.push(json!({ "text": text }));
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish,
            "index": 0,
        }],
        "usageMetadata": usage_metadata(usage),
    })
}

pub fn stream_chunk(text: &str, thought: bool) -> Value {
    let part = if thought {
        json!({ "text": text, "thought": true })
    } else {
        json!({ "text": text })
    };
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": [part] },
            "index": 0,
        }],
    })
}

pub async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let models = match http::discover_models(state.as_ref()).await {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => vec![crate::agent::ModelInfo {
            id: "trae".into(),
            name: "trae".into(),
        }],
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(gemini_error(&format!("model discovery failed: {e}"), 500)),
            ));
        }
    };
    let data: Vec<Value> = models
        .iter()
        .map(|m| {
            json!({
                "name": format!("models/{}", m.id),
                "displayName": m.name,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    Ok(Json(json!({ "models": data })))
}

/// Axum cannot register `{model}:generateContent`; the last segment is `{spec}`.
pub async fn route_generate_content(
    state: State<AppState>,
    Path(spec): Path<String>,
    req: Json<GenerateContentRequest>,
) -> Response {
    match split_model_action(&spec) {
        Some((model, "streamGenerateContent")) => {
            stream_generate_content(state, Path(model.to_string()), req)
                .await
                .into_response()
        }
        Some((model, "generateContent")) => generate_content(state, Path(model.to_string()), req)
            .await
            .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            Json(gemini_error("unknown generateContent method", 404)),
        )
            .into_response(),
    }
}

fn split_model_action(spec: &str) -> Option<(&str, &str)> {
    spec.rsplit_once(':')
}

pub async fn generate_content(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Json(req): Json<GenerateContentRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let _model = model;
    let blocks = req.to_prompt_blocks();
    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(gemini_error("contents must not be empty", 400)),
        ));
    }
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
            let body = generate_response(
                &outcome.text,
                if thought.is_empty() {
                    None
                } else {
                    Some(&thought)
                },
                finish_reason(&outcome.stop_reason),
                outcome.usage.as_ref(),
            );
            Ok(Json(body).into_response())
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(gemini_error(&e.to_string(), 500)),
        )),
    }
}

pub async fn stream_generate_content(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Json(req): Json<GenerateContentRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    let _model = model;
    let blocks = req.to_prompt_blocks();
    if blocks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(gemini_error("contents must not be empty", 400)),
        ));
    }

    let (tx, rx) = mpsc::channel(64);
    let cfg = state.clone();
    tokio::spawn(async move {
        let send = |data: Value| tx.try_send(Ok(Event::default().data(data.to_string())));
        let result = http::run_turn_with_callback(cfg.as_ref(), &blocks, &mut |ev| {
            match ev {
                TurnEvent::Thought(t) => {
                    let _ = send(stream_chunk(t, true));
                }
                TurnEvent::Message(t) => {
                    let _ = send(stream_chunk(t, false));
                }
            }
            Ok(())
        })
        .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "gemini stream failed");
            let _ = send(gemini_error(&e.to_string(), 500));
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_gemini_action_from_path_segment() {
        assert_eq!(
            split_model_action("gemini-2.0-flash:generateContent"),
            Some(("gemini-2.0-flash", "generateContent"))
        );
        assert_eq!(
            split_model_action("gemini-2.0-flash:streamGenerateContent"),
            Some(("gemini-2.0-flash", "streamGenerateContent"))
        );
        assert_eq!(split_model_action("gemini-2.0-flash"), None);
    }

    #[test]
    fn request_to_blocks() {
        let req: GenerateContentRequest = serde_json::from_value(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        }))
        .unwrap();
        assert!(!req.to_prompt_blocks().is_empty());
    }
}
