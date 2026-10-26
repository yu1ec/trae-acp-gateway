//! Reasoning stream sink: publish ACP `agent_thought_chunk` updates to the
//! host application.
//!
//! The gateway never fabricates thinking blocks for HTTP clients; this sink is
//! a side channel for the desktop app's thought panel. The callback runs
//! synchronously from the turn loop, so implementations must be cheap and
//! non-blocking.

use std::sync::Arc;

/// One reasoning stream update, forwarded from the ACP turn driver.
///
/// Updates carry the ACP session id so the panel can group interleaved turns
/// from concurrent requests into separate segments. Every gateway request
/// starts its own ACP session, so the id identifies one prompt turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThoughtUpdate {
    /// A prompt turn started; a new thought stream follows. `prompt` is the
    /// user's question for this turn, for the panel's per-segment title.
    ///
    /// `client_session` is the caller's conversation id, when the request
    /// carried one. `session_id` is unique per turn and cannot identify the
    /// conversation, so the panel uses `client_session` to attribute
    /// interleaved turns from concurrent clients.
    Start {
        session_id: String,
        client_session: Option<String>,
        workdir: String,
        /// Whether `workdir` came from the request (header, body, environment
        /// context, or system prompt) rather than the gateway's configured
        /// fallback. Auxiliary requests such as title generation carry no cwd,
        /// so the panel must not label their conversation by the fallback.
        workdir_reported: bool,
        prompt: String,
    },
    /// Incremental reasoning text.
    Delta { session_id: String, text: String },
    /// The prompt turn finished (including when it failed after starting).
    End { session_id: String },
}

/// Host-provided sink for thought updates.
pub type ThoughtSink = Arc<dyn Fn(ThoughtUpdate) + Send + Sync>;
