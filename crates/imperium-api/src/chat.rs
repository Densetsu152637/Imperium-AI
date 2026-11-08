//! Conversation types in the OpenAI-compatible Chat Completions wire format.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One message in a conversation, in the Chat Completions wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
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
    fn a_missing_tool_call_type_defaults_to_function() {
        let call: ToolCall =
            serde_json::from_value(json!({"id":"c1","function":{"name":"f","arguments":"{}"}}))
                .unwrap();
        assert_eq!(call.kind, "function");
    }
}
