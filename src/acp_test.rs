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
