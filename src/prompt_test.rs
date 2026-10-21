use super::{
    PromptBlock, blocks_from_content, blocks_from_gemini_parts, flatten_role_messages,
    text_from_content,
};
use serde_json::json;

#[test]
fn text_from_openai_multimodal() {
    let c = json!([
        { "type": "text", "text": "hello" },
        { "type": "text", "text": " world" }
    ]);
    assert_eq!(text_from_content(&c), "hello world");
}

#[test]
fn blocks_from_anthropic_image() {
    let c = json!([{
        "type": "image",
        "source": { "type": "base64", "media_type": "image/png", "data": "abc" }
    }]);
    let blocks = blocks_from_content(&c);
    assert!(matches!(blocks.first(), Some(PromptBlock::Image { .. })));
}

#[test]
fn gemini_parts_to_blocks() {
    let parts = vec![json!({ "text": "ping" })];
    assert!(matches!(
        blocks_from_gemini_parts(&parts)[0],
        PromptBlock::Text { .. }
    ));
}

#[test]
fn flatten_with_system() {
    let hi = json!("hi");
    let msgs = vec![("user".into(), &hi)];
    let blocks = flatten_role_messages(Some("You are helpful"), &msgs);
    assert!(!blocks.is_empty());
}
