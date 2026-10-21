//! HTTP handlers bridging protocol-specific requests to ACP turns.

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use serde_json::Value;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::agent::{AgentProcess, Policy, TurnEvent};
use crate::openai::{
    ChatRequest, chat_chunk, chat_response, error_response, finish_reason, flatten_messages,
    models_response, new_completion_id,
};
use crate::openai_responses::GatewayState;
use crate::prompt::PromptBlock;

/// Drive one full ACP turn: spawn, session/new, prompt, shutdown.
pub async fn run_turn(
    cfg: &GatewayState,
    blocks: &[PromptBlock],
) -> anyhow::Result<(crate::agent::SessionInfo, crate::acp::TurnOutcome)> {
    run_turn_with_callback(cfg, blocks, &mut |_| Ok(())).await
}

/// Drive one full ACP turn with live streaming through `on_event`.
pub async fn run_turn_with_callback(
    cfg: &GatewayState,
    blocks: &[PromptBlock],
    on_event: &mut (dyn FnMut(TurnEvent<'_>) -> anyhow::Result<()> + Send),
) -> anyhow::Result<(crate::agent::SessionInfo, crate::acp::TurnOutcome)> {
    let c = &cfg.config;
    let policy = if c.sandbox {
        Policy::Sandbox
    } else {
        Policy::ApproveAll
    };
    let mut agent = AgentProcess::spawn(&c.trae_cmd, &c.trae_args, &c.workdir).await?;
    let outcome = async {
        let info = agent.new_session(&c.workdir).await?;
        let outcome = agent
            .prompt(&info.session_id, blocks, &policy, on_event)
            .await?;
        Ok((info, outcome))
    }
    .await;
    agent.shutdown().await;
    outcome
}

/// Spawn the agent and create one session just to learn which models it exposes.
pub async fn discover_models(cfg: &GatewayState) -> anyhow::Result<Vec<crate::agent::ModelInfo>> {
    let c = &cfg.config;
    let mut agent = AgentProcess::spawn(&c.trae_cmd, &c.trae_args, &c.workdir).await?;
    let result = agent.new_session(&c.workdir).await.map(|info| info.models);
    agent.shutdown().await;
    result
}

pub async fn models(
    State(state): State<AppState>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let models = match discover_models(&state).await {
        Ok(list) if !list.is_empty() => list,
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

pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (axum::http::StatusCode, Json<Value>)>
{
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let blocks = vec![PromptBlock::text(flatten_messages(&req.messages))];
    let completion_id = new_completion_id();

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let cfg = state.clone();
    tokio::spawn(async move {
        let result = async {
            let opened = tx
                .send(Ok(Event::default().data(
                    chat_chunk(
                        &completion_id,
                        serde_json::json!({ "role": "assistant", "content": "" }),
                        None,
                        None,
                    )
                    .to_string(),
                )))
                .await
                .is_ok();
            if !opened {
                anyhow::bail!("client disconnected before first delta");
            }

            let tx = tx.clone();
            let completion_id = completion_id.clone();
            let mut client_gone = false;
            let turn = run_turn_with_callback(&cfg, &blocks, &mut |ev| {
                let ev = match ev {
                    TurnEvent::Message(text) => chat_chunk(
                        &completion_id,
                        serde_json::json!({ "content": text }),
                        None,
                        None,
                    ),
                    TurnEvent::Thought(text) => chat_chunk(
                        &completion_id,
                        serde_json::json!({ "reasoning_content": text }),
                        None,
                        None,
                    ),
                };
                match tx.try_send(Ok(Event::default().data(ev.to_string()))) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        client_gone = true;
                        anyhow::bail!("SSE buffer full; aborting turn")
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        client_gone = true;
                        anyhow::bail!("client disconnected mid-turn")
                    }
                }
            })
            .await;
            if client_gone {
                anyhow::bail!("client disconnected mid-turn");
            }
            turn
        }
        .await;

        match result {
            Ok((_, outcome)) => {
                let _ = tx
                    .send(Ok(Event::default().data(
                        chat_chunk(
                            &completion_id,
                            serde_json::json!({}),
                            Some(finish_reason(&outcome.stop_reason)),
                            outcome.usage.as_ref(),
                        )
                        .to_string(),
                    )))
                    .await;
                let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "turn failed");
                let _ = tx
                    .send(Ok(Event::default()
                        .event("error")
                        .data(error_response(&e.to_string(), 500).to_string())))
                    .await;
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

pub async fn chat_completions_buffered(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let blocks = vec![PromptBlock::text(flatten_messages(&req.messages))];
    let completion_id = new_completion_id();

    match run_turn(&state, &blocks).await {
        Ok((_, outcome)) => Ok(Json(chat_response(
            &completion_id,
            &outcome.text,
            finish_reason(&outcome.stop_reason),
            outcome.usage.as_ref(),
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
