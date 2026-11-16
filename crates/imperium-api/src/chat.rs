//! Conversation types in the OpenAI-compatible Chat Completions wire format.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One message in a conversation, in the Chat Completions wire format. A user message's
/// `images` (as `data:` URLs) are kept beside its text; [`ChatMessage::to_wire`] sends them as
/// `image_url` content parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
        /// Images for a vision model, as `data:image/...;base64,...` URLs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    /// A user message with text only.
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage::User {
            content: content.into(),
            images: Vec::new(),
        }
    }

    /// The message as sent to a Chat Completions endpoint: a user message with images has
    /// array content, its text part first, then one `image_url` part per image.
    pub fn to_wire(&self) -> Value {
        match self {
            ChatMessage::User { content, images } if !images.is_empty() => {
                let mut parts = vec![json!({"type": "text", "text": content})];
                parts.extend(
                    images
                        .iter()
                        .map(|url| json!({"type": "image_url", "image_url": {"url": url}})),
                );
                json!({"role": "user", "content": parts})
            }
            message => serde_json::to_value(message).unwrap_or(Value::Null),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments, exactly as the model produced them.
    pub arguments: String,
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

impl ToolSpec {
    /// The tool as sent in a request's `tools` array.
    pub fn to_wire(&self) -> Value {
        json!({"type": "function", "function": {
            "name": self.name,
            "description": self.description,
            "parameters": self.parameters,
        }})
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens served from the provider's cache (a subset of `prompt_tokens`).
    pub cached_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialise_in_the_chat_completions_format() {
        let assistant = ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                kind: function_kind(),
                function: FunctionCall {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            }],
        };
        assert_eq!(
            serde_json::to_value(&assistant).unwrap(),
            json!({"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]})
        );
        let tool = ChatMessage::Tool {
            tool_call_id: "c1".into(),
            content: "ok".into(),
        };
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({"role":"tool","tool_call_id":"c1","content":"ok"})
        );
    }

    #[test]
    fn user_images_go_as_content_parts_and_text_only_messages_stay_strings() {
        let plain = ChatMessage::user("hi");
        assert_eq!(plain.to_wire(), json!({"role": "user", "content": "hi"}));
        let with_image = ChatMessage::User {
            content: "what is this?".into(),
            images: vec!["data:image/png;base64,AA==".into()],
        };
        assert_eq!(
            with_image.to_wire(),
            json!({"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}}
            ]})
        );
        // Saved conversations keep the images beside the text and read back.
        let saved = serde_json::to_value(&with_image).unwrap();
        assert_eq!(saved["images"][0], "data:image/png;base64,AA==");
        assert_eq!(
            serde_json::from_value::<ChatMessage>(saved).unwrap(),
            with_image
        );
        let old: ChatMessage =
            serde_json::from_value(json!({"role": "user", "content": "x"})).unwrap();
        assert_eq!(old, ChatMessage::user("x"));
    }

    #[test]
    fn a_missing_tool_call_type_defaults_to_function() {
        let call: ToolCall =
            serde_json::from_value(json!({"id":"c1","function":{"name":"f","arguments":"{}"}}))
                .unwrap();
        assert_eq!(call.kind, "function");
    }
}
