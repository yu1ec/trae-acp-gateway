//! Protocol-agnostic prompt blocks sent to ACP `session/prompt`.

use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub enum PromptBlock {
    Text { text: String },
    Image {
        mime_type: String,
        data_b64: String,
    },
}

impl PromptBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    pub fn to_acp_json(&self) -> Value {
        match self {
            PromptBlock::Text { text } => json!({ "type": "text", "text": text }),
            PromptBlock::Image { mime_type, data_b64 } => json!({
                "type": "image",
                "mimeType": mime_type,
                "data": data_b64,
            }),
        }
    }
}

pub fn blocks_to_acp(blocks: &[PromptBlock]) -> Vec<Value> {
    blocks.iter().map(PromptBlock::to_acp_json).collect()
}

/// Extract text from string or `{type:"text",text}` arrays (OpenAI / Anthropic / Gemini).
pub fn text_from_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                let t = p.get("type").and_then(|v| v.as_str())?;
                match t {
                    "text" => p.get("text").and_then(|v| v.as_str()),
                    "input_text" => p.get("text").and_then(|v| v.as_str()),
                    "output_text" => p.get("text").and_then(|v| v.as_str()),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Turn message `content` JSON into ACP blocks (text + inline images where recognized).
pub fn blocks_from_content(content: &serde_json::Value) -> Vec<PromptBlock> {
    match content {
        serde_json::Value::String(s) if !s.is_empty() => vec![PromptBlock::text(s.clone())],
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| block_from_part(p))
            .collect(),
        _ => Vec::new(),
    }
}

fn block_from_part(part: &serde_json::Value) -> Option<PromptBlock> {
    let t = part.get("type").and_then(|v| v.as_str())?;
    match t {
        "text" => part
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| PromptBlock::text(s.to_string())),
        "input_text" | "output_text" => part
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| PromptBlock::text(s.to_string())),
        "image" => anthropic_image(part),
        "input_image" => openai_input_image(part),
        _ => None,
    }
}

fn anthropic_image(part: &serde_json::Value) -> Option<PromptBlock> {
    let source = part.get("source")?;
    if source.get("type").and_then(|v| v.as_str()) != Some("base64") {
        return None;
    }
    let mime = source.get("media_type").and_then(|v| v.as_str())?;
    let data = source.get("data").and_then(|v| v.as_str())?;
    Some(PromptBlock::Image {
        mime_type: mime.to_string(),
        data_b64: data.to_string(),
    })
}

fn openai_input_image(part: &serde_json::Value) -> Option<PromptBlock> {
    if let Some(url) = part.get("image_url").and_then(|u| u.get("url")).and_then(|v| v.as_str()) {
        if let Some(rest) = url.strip_prefix("data:") {
            if let Some((mime, data)) = rest.split_once(";base64,") {
                return Some(PromptBlock::Image {
                    mime_type: mime.to_string(),
                    data_b64: data.to_string(),
                });
            }
        }
    }
    None
}

/// Gemini `parts` array → blocks.
pub fn blocks_from_gemini_parts(parts: &[serde_json::Value]) -> Vec<PromptBlock> {
    parts
        .iter()
        .filter_map(|p| {
            if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                if text.is_empty() {
                    return None;
                }
                return Some(PromptBlock::text(text.to_string()));
            }
            let inline = p.get("inlineData")?;
            let mime = inline.get("mimeType").and_then(|v| v.as_str())?;
            let data = inline.get("data").and_then(|v| v.as_str())?;
            Some(PromptBlock::Image {
                mime_type: mime.to_string(),
                data_b64: data.to_string(),
            })
        })
        .collect()
}

pub fn flatten_role_messages(
    system_prefix: Option<&str>,
    messages: &[(String, &serde_json::Value)],
) -> Vec<PromptBlock> {
    let mut blocks = Vec::new();
    if let Some(sys) = system_prefix.filter(|s| !s.is_empty()) {
        blocks.push(PromptBlock::text(format!("{sys}\n")));
    }
    for (role, content) in messages {
        let part_blocks = blocks_from_content(content);
        if part_blocks.is_empty() {
            continue;
        }
        let prefix = match role.as_str() {
            "assistant" => "[assistant]: ",
            "user" => "[user]: ",
            _ => "[user]: ",
        };
        blocks.push(PromptBlock::text(prefix.to_string()));
        blocks.extend(part_blocks);
        blocks.push(PromptBlock::text("\n".to_string()));
    }
    if blocks.is_empty() && system_prefix.is_some() {
        blocks.push(PromptBlock::text(
            system_prefix.unwrap_or("").to_string(),
        ));
    }
    blocks
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod prompt_tests;
