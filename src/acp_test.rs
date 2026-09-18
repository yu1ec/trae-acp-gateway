use crate::acp::Update;

#[test]
fn agent_message_chunk_deserializes() {
    let raw = serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": "Echo: hi" }
    });
    let upd: Update = serde_json::from_value(raw).unwrap();
    match upd {
        Update::AgentMessageChunk { content } => {
            assert_eq!(content.r#type, "text");
            assert_eq!(content.text, "Echo: hi");
        }
        _ => panic!("expected AgentMessageChunk"),
    }
}

#[test]
fn agent_thought_chunk_deserializes() {
    let raw = serde_json::json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": { "type": "text", "text": "pondering" }
    });
    let upd: Update = serde_json::from_value(raw).unwrap();
    match upd {
        Update::AgentThoughtChunk { content } => assert_eq!(content.text, "pondering"),
        _ => panic!("expected AgentThoughtChunk"),
    }
}

#[test]
fn usage_struct_maps_acp_fields() {
    let u: crate::acp::Usage = serde_json::from_value(serde_json::json!({
        "totalTokens": 21330, "inputTokens": 21311, "outputTokens": 19,
        "thoughtTokens": 16, "cachedReadTokens": 7, "cachedWriteTokens": 0
    }))
    .unwrap();
    assert_eq!(u.total_tokens, 21330);
    assert_eq!(u.input_tokens, 21311);
    assert_eq!(u.output_tokens, 19);
    assert_eq!(u.thought_tokens, 16);
    assert_eq!(u.cached_read_tokens, 7);
    // Unknown ACP fields (e.g. cachedWriteTokens) are ignored.
}
