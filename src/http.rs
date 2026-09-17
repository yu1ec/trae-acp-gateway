//! HTTP handlers bridging OpenAI-compatible requests to ACP turns.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use serde_json::Value;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::{AgentProcess, Policy};
use crate::config::Config;
use crate::openai::{
    ChatRequest, chat_chunk, chat_response, error_response, finish_reason, flatten_messages,
    models_response, new_completion_id,
};

pub type SharedConfig = Arc<Config>;

pub async fn chat_completions(
    State(cfg): State<SharedConfig>,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (axum::http::StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let prompt = flatten_messages(&req.messages);
    let completion_id = new_completion_id();

    // Run the whole turn in a task; stream its output as SSE events. The SSE
    // body is produced regardless of `stream` — clients that asked for a
    // single JSON body use the buffered variant below.
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let cfg = cfg.clone();
    tokio::spawn(async move {
        let result = run_turn(&cfg, &prompt).await;
        let stream = |ev: Event| async {
            if tx.send(Ok(ev)).await.is_err() {
                tracing::debug!("client disconnected; dropping turn output");
            }
        };

        match result {
            Ok((_, outcome)) => {
                stream(Event::default().data(
                    chat_chunk(
                        &completion_id,
                        serde_json::json!({ "role": "assistant", "content": "" }),
                        None,
                    )
                    .to_string(),
                ))
                .await;
                stream(Event::default().data(
                    chat_chunk(
                        &completion_id,
                        serde_json::json!({ "content": outcome.text }),
                        None,
                    )
                    .to_string(),
                ))
                .await;
                stream(Event::default().data(
                    chat_chunk(
                        &completion_id,
                        serde_json::json!({}),
                        Some(finish_reason(&outcome.stop_reason)),
                    )
                    .to_string(),
                ))
                .await;
                let _ = tx
                    .send(Ok(Event::default().data("[DONE]")))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = %e, "turn failed");
                let _ = tx
                    .send(Ok(Event::default().event("error").data(
                        error_response(&e.to_string(), 500).to_string(),
                    )))
                    .await;
            }
        }
    });

    let _ = req.stream; // both modes share the SSE path below for now
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

/// Drive one full ACP turn: spawn, session/new, prompt, shutdown.
async fn run_turn(
    cfg: &Config,
    prompt: &str,
) -> anyhow::Result<(crate::agent::SessionInfo, crate::acp::TurnOutcome)> {
    let mut agent = AgentProcess::spawn(&cfg.trae_cmd, &cfg.trae_args, &cfg.workdir).await?;
    let outcome = async {
        let info = agent.new_session(&cfg.workdir).await?;
        let outcome = agent.prompt(&info.session_id, prompt, &Policy::ApproveAll).await?;
        Ok((info, outcome))
    }
    .await;
    agent.shutdown().await;
    outcome
}

/// Spawn the agent and create one session just to learn which models it
/// exposes. ACP v1 has no standalone "list models" method — agents advertise
/// selectable models via `configOptions` on `session/new`.
async fn discover_models(cfg: &Config) -> anyhow::Result<Vec<crate::agent::ModelInfo>> {
    let mut agent = AgentProcess::spawn(&cfg.trae_cmd, &cfg.trae_args, &cfg.workdir).await?;
    let result = agent.new_session(&cfg.workdir).await.map(|info| info.models);
    agent.shutdown().await;
    result
}

pub async fn models(State(cfg): State<SharedConfig>) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let models = match discover_models(&cfg).await {
        Ok(list) if !list.is_empty() => list,
        // Agent reachable but exposes no model selector: report the gateway's
        // placeholder so clients still see a usable entry.
        Ok(_) => {
            tracing::debug!("agent advertised no models; falling back to placeholder");
            vec![crate::agent::ModelInfo {
                id: crate::openai::MODEL_NAME.to_string(),
                name: crate::openai::MODEL_NAME.to_string(),
            }]
        }
        Err(e) => {
            tracing::error!(error = %e, "model discovery failed");
            return Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response(&format!("model discovery failed: {e}"), 500)),
            ));
        }
    };
    Ok(Json(models_response(&models)))
}

/// Non-streaming handler: same turn, buffered into one JSON body.
pub async fn chat_completions_buffered(
    State(cfg): State<SharedConfig>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let prompt = flatten_messages(&req.messages);
    let completion_id = new_completion_id();

    match run_turn(&cfg, &prompt).await {
        Ok((_, outcome)) => Ok(Json(chat_response(
            &completion_id,
            &outcome.text,
            finish_reason(&outcome.stop_reason),
        ))),
        Err(e) => {
            tracing::error!(error = %e, "turn failed");
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_response(&e.to_string(), 500)),
            ))
        }
    }
}
