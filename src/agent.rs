//! Agent subprocess driver: spawn the ACP agent, speak JSON-RPC over its
//! stdio, run one prompt turn to completion.
//!
//! Ordering invariant: the stdout pump pushes every inbound message into ONE
//! unbounded event channel, so events are processed strictly in the order the
//! agent wrote them. ACP agents emit all `session/update` notifications for a
//! turn before the `session/prompt` response, so collecting text by draining
//! the event stream until the response is race-free.

use std::process::Stdio;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::acp::{Error, Id, Inbound, PromptResponse, TurnOutcome, Update};

/// Every inbound message the pump can deliver, in agent order.
enum Event {
    /// Response to one of OUR requests.
    Response(Id, Result<Value, Error>),
    /// `session/update` notification.
    Update(Update),
    /// Agent-initiated request that MUST be answered.
    AgentRequest(Id, String, Value),
}

/// How the driver answers agent-initiated requests.
pub enum Policy {
    /// Auto-approve permission requests by picking the first `allow_*` option.
    ApproveAll,
}

impl Policy {
    fn answer(&self, method: &str, params: &Value) -> Value {
        match (self, method) {
            (Policy::ApproveAll, "session/request_permission") => {
                // Pick the first option whose kind is allow_once/allow_always,
                // falling back to the first listed option. Never hardcode ids.
                let options = params
                    .get("options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let chosen = options
                    .iter()
                    .find(|o| {
                        o.get("kind").and_then(Value::as_str)
                            .is_some_and(|k| k.starts_with("allow"))
                    })
                    .or_else(|| options.first());
                match chosen.and_then(|o| o.get("optionId").and_then(Value::as_str)) {
                    Some(option_id) => json!({
                        "outcome": { "outcome": "selected", "optionId": option_id }
                    }),
                    // Malformed request: no options at all. Fail visibly.
                    None => error_result(-32602, "no permission options provided"),
                }
            }
            // fs/terminal/elicitation: we advertised these capabilities as off.
            // A compliant agent never sends them; answer with a JSON-RPC error
            // so a non-compliant one fails fast instead of hanging.
            (_, m) => error_result(-32601, &format!("gateway does not support `{m}`")),
        }
    }
}

fn error_result(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

/// What `session/new` tells us about a fresh session.
#[derive(Debug)]
pub struct SessionInfo {
    pub session_id: String,
    /// Models the agent advertises as selectable for this session, in agent
    /// order. Empty when the agent exposes no model selector.
    pub models: Vec<ModelInfo>,
}

/// One selectable model from a `configOptions` entry with `category: "model"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// Stable value id — what an OpenAI client would send as `model`.
    pub id: String,
    pub name: String,
}

/// Pull `(id, name)` pairs from every select-type `configOptions` entry whose
/// category is `model`. Per the ACP spec (session-config-options) this is the
/// standard place agents advertise their models. Unknown/missing categories
/// and non-select entries are ignored.
fn extract_models(session_new_result: &Value) -> Vec<ModelInfo> {
    let Some(options) = session_new_result.get("configOptions").and_then(Value::as_array) else {
        return Vec::new();
    };
    options
        .iter()
        .filter(|opt| {
            opt.get("category").and_then(Value::as_str) == Some("model")
                && opt.get("type").and_then(Value::as_str) == Some("select")
        })
        .filter_map(|opt| opt.get("options").and_then(Value::as_array))
        .flatten()
        .filter_map(|entry| {
            let id = entry.get("value").and_then(Value::as_str)?;
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string();
            Some(ModelInfo { id: id.to_string(), name })
        })
        .collect()
}

/// One ACP connection = one spawned agent process = one HTTP request's lifetime.
pub struct AgentProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    /// All inbound messages, strictly in agent emission order.
    events: mpsc::UnboundedReceiver<Event>,
    next_id: i64,
    /// Updates seen while servicing a different request; prepended to the
    /// next turn's text (agents only emit them inside a turn, but ordering
    /// with respect to a stray response must hold anyway).
    queued_updates: Vec<Update>,
}

impl AgentProcess {
    /// Spawn the agent and run the `initialize` handshake.
    pub async fn spawn(cmd: &str, args: &[String], cwd: &str) -> anyhow::Result<Self> {
        let mut child = Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Agent's own logs go to our stderr, never onto the RPC channel.
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning agent `{cmd} {}`", args.join(" ")))?;

        let stdout = child.stdout.take().context("agent stdout not captured")?;
        let stdin = child.stdin.take().context("agent stdin not captured")?;

        let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

        // Pump: read agent stdout lines, classify, push IN ORDER.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                tracing::debug!(line, "<- agent");
                let event = match serde_json::from_str::<Inbound>(line) {
                    Ok(Inbound::Response { id, result, error }) => {
                        let payload = match (result, error) {
                            (Some(r), _) => Ok(r),
                            (None, Some(e)) => Err(e),
                            (None, None) => Ok(Value::Null),
                        };
                        Event::Response(id, payload)
                    }
                    Ok(Inbound::Request { id, method, params }) => {
                        Event::AgentRequest(id, method, params)
                    }
                    Ok(Inbound::Notification { method, params }) => {
                        if method != "session/update" {
                            continue; // ignorable notification
                        }
                        // The discriminator lives at params.update.sessionUpdate.
                        let update = params
                            .get("update")
                            .cloned()
                            .unwrap_or(Value::Null);
                        Event::Update(
                            serde_json::from_value::<Update>(update).unwrap_or(Update::Other),
                        )
                    }
                    Err(_) => {
                        tracing::warn!(line, "unparseable line from agent stdout");
                        continue;
                    }
                };
                if event_tx.send(event).is_err() {
                    break; // owner gone; child dies via kill_on_drop
                }
            }
        });

        let mut agent = Self {
            stdin,
            child,
            events: event_rx,
            next_id: 0,
            queued_updates: Vec::new(),
        };
        agent.initialize(cwd).await?;
        Ok(agent)
    }

    async fn initialize(&mut self, _cwd: &str) -> anyhow::Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                },
                "clientInfo": { "name": "trae_acp_gateway", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await
        .map(|_| ())
    }

    /// Send a request and await its response, servicing interleaved agent
    /// requests and buffering any updates seen meanwhile.
    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.send_request(method, params).await?;
        loop {
            match self.events.recv().await.context("agent connection closed before responding")? {
                Event::Response(resp_id, payload) => {
                    if resp_id != id {
                        bail!("response id mismatch: got {resp_id:?}, want {id:?}");
                    }
                    return payload
                        .map_err(|e| anyhow::anyhow!("agent error {} ({}): {}", e.code, e.message, method));
                }
                Event::Update(u) => self.queued_updates.push(u),
                Event::AgentRequest(req_id, req_method, req_params) => {
                    let result = Policy::ApproveAll.answer(&req_method, &req_params);
                    self.reply(req_id, result).await?;
                }
            }
        }
    }

    async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<Id> {
        let id = Id::Num(self.next_id);
        self.next_id += 1;
        let frame = json!({
            "jsonrpc": "2.0", "id": id.to_value(), "method": method, "params": params,
        });
        tracing::debug!(%method, line = %frame, "-> agent");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .context("writing to agent stdin")?;
        Ok(id)
    }

    /// Create a session and report what the agent exposes in its
    /// `session/new` response: the session id plus, when the agent advertises
    /// them, its selectable models (`configOptions` entries with
    /// `category: "model"`). Returning both avoids a second round-trip for
    /// the common spawn → session/new sequence.
    pub async fn new_session(&mut self, cwd: &str) -> anyhow::Result<SessionInfo> {
        let result = self
            .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
            .await?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("agent returned no sessionId")?;
        let models = extract_models(&result);
        Ok(SessionInfo { session_id, models })
    }

    /// Run one prompt turn: send `session/prompt`, drain agent updates in
    /// order (answering agent-initiated requests via `policy`), return the
    /// collected text when the prompt response arrives.
    pub async fn prompt(
        &mut self,
        session_id: &str,
        text: &str,
        policy: &Policy,
    ) -> anyhow::Result<TurnOutcome> {
        let prompt_id = self
            .send_request(
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": text }],
                }),
            )
            .await?;

        let mut text_out = String::new();
        // Updates that arrived before this call (shouldn't happen, but order
        // is order): they belong to this conversation's stream.
        for u in std::mem::take(&mut self.queued_updates) {
            Self::collect(u, &mut text_out);
        }

        loop {
            match self.events.recv().await.context("agent connection closed mid-turn")? {
                Event::Update(u) => Self::collect(u, &mut text_out),
                Event::AgentRequest(req_id, req_method, req_params) => {
                    let result = policy.answer(&req_method, &req_params);
                    self.reply(req_id, result).await?;
                }
                Event::Response(resp_id, payload) => {
                    if resp_id != prompt_id {
                        bail!("prompt response id mismatch: got {resp_id:?}, want {prompt_id:?}");
                    }
                    let resp: PromptResponse = serde_json::from_value(
                        payload.map_err(|e| anyhow::anyhow!("prompt error {}: {}", e.code, e.message))?,
                    )
                    .unwrap_or(PromptResponse { stop_reason: "unknown".into() });
                    return Ok(TurnOutcome { text: text_out, stop_reason: resp.stop_reason });
                }
            }
        }
    }

    fn collect(update: Update, text_out: &mut String) {
        if let Update::AgentMessageChunk { content } = update
            && content.r#type == "text"
        {
            text_out.push_str(&content.text);
        }
    }

    async fn reply(&mut self, id: Id, result: Value) -> anyhow::Result<()> {
        let frame = json!({ "jsonrpc": "2.0", "id": id.to_value(), "result": result });
        tracing::debug!(line = %frame, "-> agent");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .context("writing to agent stdin")?;
        Ok(())
    }

    /// Kill the agent process. `kill_on_drop` is the backstop; this makes
    /// cleanup deterministic at request end.
    pub async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::extract_models;
    use serde_json::json;

    #[test]
    fn extracts_model_category_select_options() {
        let result = json!({
            "sessionId": "s1",
            "configOptions": [
                {
                    "id": "mode", "name": "Session Mode", "category": "mode",
                    "type": "select", "currentValue": "ask",
                    "options": [{"value": "ask", "name": "Ask"}]
                },
                {
                    "id": "model", "name": "Model", "category": "model",
                    "type": "select", "currentValue": "glm-4.6",
                    "options": [
                        {"value": "glm-4.6", "name": "GLM-4.6"},
                        {"value": "glm-4.5-air", "name": "GLM-4.5-Air", "description": "cheaper"}
                    ]
                }
            ]
        });
        let models = extract_models(&result);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "glm-4.6");
        assert_eq!(models[0].name, "GLM-4.6");
        assert_eq!(models[1].id, "glm-4.5-air");
        assert_eq!(models[1].name, "GLM-4.5-Air");
    }

    #[test]
    fn ignores_non_model_and_non_select_options() {
        let result = json!({
            "sessionId": "s1",
            "configOptions": [
                {
                    // category present but boolean type: not a model list
                    "id": "model", "name": "Model", "category": "model",
                    "type": "boolean", "currentValue": true
                },
                {
                    // select but uncategorized: not a model list
                    "id": "mode", "name": "Mode", "type": "select",
                    "currentValue": "ask",
                    "options": [{"value": "ask", "name": "Ask"}]
                }
            ]
        });
        assert!(extract_models(&result).is_empty());
    }

    #[test]
    fn missing_config_options_yields_empty() {
        assert!(extract_models(&json!({"sessionId": "s1"})).is_empty());
        assert!(extract_models(&json!({"sessionId": "s1", "configOptions": null})).is_empty());
    }
}
