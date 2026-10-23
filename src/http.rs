//! HTTP handlers bridging protocol-specific requests to ACP turns.

use anyhow::Context as _;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_core::Stream;
use serde_json::Value;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::AppState;
use crate::agent::{AgentProcess, Policy, TurnEvent};
use crate::openai::{
    ChatRequest, chat_chunk, chat_response, error_response, finish_reason, flatten_messages,
    models_response, new_completion_id,
};
use crate::openai_responses::GatewayState;
use crate::prompt::PromptBlock;
use crate::thought::ThoughtUpdate;

/// Headers a client may use to report its own conversation id.
///
/// Claude Code sends `X-Claude-Code-Session-Id` on every request; the generic
/// aliases let other clients opt in. Same value across turns = same
/// conversation, which is what lets the thought panel separate interleaved
/// sessions (one gateway request is exactly one turn, so the ACP session id
/// changes every time and cannot fill this role).
const SESSION_HEADERS: &[&str] = &[
    "x-claude-code-session-id",
    "x-session-id",
    "x-conversation-id",
];

/// Resolve the caller's conversation id from headers, then from known body
/// metadata shapes. Returns `None` when the client reports nothing; the panel
/// then shows an explicit "unmarked" badge instead of merging unrelated turns.
pub fn client_session(headers: &HeaderMap, metadata: Option<&Value>) -> Option<String> {
    for name in SESSION_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    let metadata = metadata?;
    for key in [
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
    ] {
        if let Some(value) = metadata.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    // Claude Code's older clients embed the session UUID in
    // `metadata.user_id` as `user_<hash>_account_<id>_session_<uuid>`. Only
    // keep the session part: the rest is account-scoped and would merge
    // unrelated conversations.
    let user_id = metadata.get("user_id").and_then(Value::as_str)?;
    user_id
        .split_once("_session_")
        .map(|(_, session)| session.trim())
        .filter(|session| !session.is_empty())
        .map(str::to_string)
}

/// Resolve the workdir for one request.
pub fn resolve_workdir(
    cfg: &GatewayState,
    headers: &HeaderMap,
    body: Option<&str>,
    environment_context: Option<&str>,
    system_prompt: Option<&str>,
) -> anyhow::Result<String> {
    let header = headers
        .get(crate::config::WORKDIR_HEADER)
        .map(|v| {
            v.to_str()
                .context("X-Trae-Workdir header is not valid UTF-8")
        })
        .transpose()?;
    let c = &cfg.config;
    let resolved = crate::config::resolve_request_workdir(
        header,
        body,
        environment_context,
        system_prompt,
        &c.workdir,
        &c.allowed_workdir_root,
    )?;
    tracing::debug!(
        workdir = %resolved.path,
        source = resolved.source.as_str(),
        "resolved request workdir"
    );
    Ok(resolved.path)
}

/// Drive one full ACP turn: spawn, session/new, prompt, shutdown.
pub async fn run_turn(
    cfg: &GatewayState,
    workdir: &str,
    client_session: Option<&str>,
    blocks: &[PromptBlock],
) -> anyhow::Result<(crate::agent::SessionInfo, crate::acp::TurnOutcome)> {
    run_turn_with_callback(cfg, workdir, client_session, blocks, &mut |_| Ok(())).await
}

/// Best-effort one-line hint of what this turn asked for, shown as the
/// segment title in the thought panel. OpenAI chat flattens the whole
/// conversation into a single block, so pull out the last `[user]:` entry;
/// every other protocol sends one block per message part.
fn prompt_hint(blocks: &[PromptBlock]) -> String {
    let text = match blocks {
        [PromptBlock::Text { text }] => last_user_turn(text).unwrap_or(text.as_str()),
        _ => blocks
            .iter()
            .rev()
            .find_map(|block| match block {
                PromptBlock::Text { text }
                    if !text.trim().is_empty() && !is_injected_context(text) =>
                {
                    Some(text.as_str())
                }
                _ => None,
            })
            .or_else(|| last_non_empty_text(blocks))
            .unwrap_or(""),
    };
    text.trim().chars().take(600).collect()
}

fn last_non_empty_text(blocks: &[PromptBlock]) -> Option<&str> {
    blocks.iter().rev().find_map(|block| match block {
        PromptBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        _ => None,
    })
}

/// Hosts inject environment and reminder material as ordinary text parts. Those
/// blocks are huge, so using one as the turn title hides the actual question;
/// prefer any other text part.
fn is_injected_context(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("# Environment")
        || t.starts_with("<environment")
        || t.starts_with("<system-reminder>")
        || t.starts_with("You have been invoked in the following environment")
}

fn last_user_turn(text: &str) -> Option<&str> {
    const MARKER: &str = "[user]: ";
    let rest = &text[text.rfind(MARKER)? + MARKER.len()..];
    Some(match rest.find("\n[assistant]: ") {
        Some(end) => &rest[..end],
        None => rest,
    })
}

/// Drive one full ACP turn with live streaming through `on_event`.
pub async fn run_turn_with_callback(
    cfg: &GatewayState,
    workdir: &str,
    client_session: Option<&str>,
    blocks: &[PromptBlock],
    on_event: &mut (dyn FnMut(TurnEvent<'_>) -> anyhow::Result<()> + Send),
) -> anyhow::Result<(crate::agent::SessionInfo, crate::acp::TurnOutcome)> {
    let c = &cfg.config;
    // Hold the busy marker for the whole turn so the host's tray indicator
    // covers buffered and streaming responses alike.
    let _activity = cfg.begin_activity();
    let policy = if c.sandbox {
        Policy::Sandbox
    } else {
        Policy::ApproveAll
    };
    // The process cwd and the ACP session cwd must agree: tools resolve
    // relative paths against the session, while the agent itself starts here.
    let mut agent = AgentProcess::spawn(&c.trae_cmd, &c.trae_args, workdir).await?;
    // Fan reasoning chunks out to the host panel in addition to whatever the
    // protocol handler does with them. This is independent of the client's
    // `thinking` flag: the panel always sees the real ACP stream.
    let sink = cfg.thought_sink.clone();
    let prompt = prompt_hint(blocks);
    let client_session = client_session.map(str::to_string);
    let mut thought_started = false;
    // Kept outside the async block so the panel still gets an `End` when the
    // turn fails after its first reasoning chunk.
    let mut thought_session: Option<String> = None;
    let outcome = {
        async {
            let info = agent.new_session(workdir).await?;
            let session_id = info.session_id.clone();
            thought_session = Some(session_id.clone());
            let mut forward = |ev: TurnEvent<'_>| {
                if let (Some(sink), TurnEvent::Thought(text)) = (&sink, &ev) {
                    if !thought_started {
                        sink(ThoughtUpdate::Start {
                            session_id: session_id.clone(),
                            client_session: client_session.clone(),
                            workdir: workdir.to_string(),
                            prompt: prompt.clone(),
                        });
                        thought_started = true;
                    }
                    sink(ThoughtUpdate::Delta {
                        session_id: session_id.clone(),
                        text: (*text).to_string(),
                    });
                }
                on_event(ev)
            };
            let outcome = agent
                .prompt(&info.session_id, blocks, &policy, &mut forward)
                .await?;
            Ok((info, outcome))
        }
        .await
    };
    agent.shutdown().await;
    if let (true, Some(session_id)) = (thought_started, thought_session) {
        if let Some(sink) = &sink {
            sink(ThoughtUpdate::End { session_id });
        }
    }
    outcome
}

/// Spawn the agent and create one session just to learn which models it exposes.
pub async fn discover_models(cfg: &GatewayState) -> anyhow::Result<Vec<crate::agent::ModelInfo>> {
    let _activity = cfg.begin_activity();
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
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (axum::http::StatusCode, Json<Value>)>
{
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let workdir =
        resolve_workdir(&state, &headers, req.workdir.as_deref(), None, None).map_err(|e| {
            tracing::warn!(error = %e, "invalid workdir");
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(error_response(&format!("invalid workdir: {e}"), 400)),
            )
        })?;
    let blocks = vec![PromptBlock::text(flatten_messages(&req.messages))];
    let client_session = client_session(&headers, None);
    let completion_id = new_completion_id();

    // Unbounded on purpose: the turn callback is synchronous, so it cannot
    // await capacity. A bounded queue + try_send aborted the whole turn
    // whenever the client was briefly slow.
    let (tx, rx) = mpsc::unbounded_channel::<Result<Event, Infallible>>();
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
                .is_ok();
            if !opened {
                anyhow::bail!("client disconnected before first delta");
            }

            let completion_id = completion_id.clone();
            let mut client_gone = false;
            let turn = run_turn_with_callback(
                &cfg,
                &workdir,
                client_session.as_deref(),
                &blocks,
                &mut |ev| {
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
                    match tx.send(Ok(Event::default().data(ev.to_string()))) {
                        Ok(()) => Ok(()),
                        Err(_) => {
                            client_gone = true;
                            anyhow::bail!("client disconnected mid-turn")
                        }
                    }
                },
            )
            .await;
            if client_gone {
                anyhow::bail!("client disconnected mid-turn");
            }
            turn
        }
        .await;

        match result {
            Ok((_, outcome)) => {
                let _ = tx.send(Ok(Event::default().data(
                    chat_chunk(
                        &completion_id,
                        serde_json::json!({}),
                        Some(finish_reason(&outcome.stop_reason)),
                        outcome.usage.as_ref(),
                    )
                    .to_string(),
                )));
                let _ = tx.send(Ok(Event::default().data("[DONE]")));
            }
            Err(e) => {
                tracing::error!(error = %e, "turn failed");
                let _ = tx.send(Ok(Event::default()
                    .event("error")
                    .data(error_response(&e.to_string(), 500).to_string())));
            }
        }
    });

    Ok(Sse::new(UnboundedReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

pub async fn chat_completions_buffered(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if req.messages.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(error_response("messages must not be empty", 400)),
        ));
    }
    let workdir =
        resolve_workdir(&state, &headers, req.workdir.as_deref(), None, None).map_err(|e| {
            tracing::warn!(error = %e, "invalid workdir");
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(error_response(&format!("invalid workdir: {e}"), 400)),
            )
        })?;
    let blocks = vec![PromptBlock::text(flatten_messages(&req.messages))];
    let client_session = client_session(&headers, None);
    let completion_id = new_completion_id();

    match run_turn(&state, &workdir, client_session.as_deref(), &blocks).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn client_session_reads_claude_code_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("01a1667d-f926-7270-ade5-4b4ef94a9829"),
        );
        assert_eq!(
            client_session(&headers, None).as_deref(),
            Some("01a1667d-f926-7270-ade5-4b4ef94a9829")
        );
    }

    #[test]
    fn client_session_falls_back_to_metadata() {
        let headers = HeaderMap::new();
        let metadata = serde_json::json!({ "session_id": "conversation-7" });
        assert_eq!(
            client_session(&headers, Some(&metadata)).as_deref(),
            Some("conversation-7")
        );
    }

    #[test]
    fn client_session_extracts_claude_user_id_suffix() {
        let headers = HeaderMap::new();
        let metadata = serde_json::json!({
            "user_id": "user_abc_account_def_session_01a1667d-f926-7270"
        });
        assert_eq!(
            client_session(&headers, Some(&metadata)).as_deref(),
            Some("01a1667d-f926-7270")
        );
    }

    #[test]
    fn client_session_is_none_without_ids() {
        let headers = HeaderMap::new();
        let metadata = serde_json::json!({ "user_id": "user_abc_account_def" });
        assert_eq!(client_session(&headers, Some(&metadata)), None);
        assert_eq!(client_session(&headers, None), None);
    }
}
