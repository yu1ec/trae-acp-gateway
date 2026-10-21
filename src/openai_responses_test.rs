use super::*;
use serde_json::json;

#[test]
fn build_response_shape() {
    let body = build_response("resp_1", "gpt-test", "hello", None);
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["content"][0]["text"], "hello");
}

#[test]
fn previous_response_id_adds_context() {
    let store = ResponseStore::default();
    store.insert(
        "resp_prev".into(),
        json!({ "id": "resp_prev" }),
        "User: hi\nAssistant: there\n".into(),
    );
    let req: ResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-test",
        "input": "follow up",
        "previous_response_id": "resp_prev"
    }))
    .unwrap();
    let blocks = req.to_prompt_blocks(&store);
    let text: String = blocks
        .iter()
        .filter_map(|b| match b {
            PromptBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("Previous conversation"));
}
