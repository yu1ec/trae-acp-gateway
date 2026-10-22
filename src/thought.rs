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
    Start {
        session_id: String,
        workdir: String,
        prompt: String,
    },
    /// Incremental reasoning text.
    Delta { session_id: String, text: String },
    /// The prompt turn finished (including when it failed after starting).
    End { session_id: String },
}

/// Host-provided sink for thought updates.
pub type ThoughtSink = Arc<dyn Fn(ThoughtUpdate) + Send + Sync>;
