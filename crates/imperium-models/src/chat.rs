use std::fmt;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sse::DataLines;

/// An OpenAI-compatible endpoint, such as `https://openrouter.ai/api/v1` or
/// `http://127.0.0.1:8080/v1`.
#[derive(Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: Option<String>,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

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

#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub tools: &'a [ToolSpec],
}

/// The assembled result of one streamed completion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// Absent when the endpoint does not report usage for streamed responses.
    pub usage: Option<Usage>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("request to the model endpoint failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("model endpoint returned HTTP {status}: {body}")]
    Status {
        status: u16,
        body: String,
        /// Seconds until the provider says to try again, from `Retry-After` or a
        /// rate-limit reset header.
        retry_after_s: Option<u64>,
    },
    #[error("could not decode the model's stream: {0}")]
    Decode(String),
}

impl ModelError {
    /// Whether the provider refused for now (rate or usage limit, overload) rather than for
    /// good, so waiting and retrying can succeed.
    pub fn is_usage_limit(&self) -> bool {
        matches!(
            self,
            ModelError::Status {
                status: 429 | 503 | 529,
                ..
            }
        )
    }

    /// When the provider says to retry, in seconds, if it said.
    pub fn retry_after_s(&self) -> Option<u64> {
        match self {
            ModelError::Status { retry_after_s, .. } => *retry_after_s,
            _ => None,
        }
    }
}

/// Seconds to wait from `Retry-After` (seconds) or common reset headers (seconds, or an RFC
/// 3339 time).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let names = [
        "retry-after",
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
        "x-ratelimit-reset",
        "anthropic-ratelimit-requests-reset",
        "anthropic-ratelimit-tokens-reset",
    ];
    names
        .iter()
        .filter_map(|name| headers.get(*name)?.to_str().ok())
        .filter_map(parse_wait)
        .max()
}

fn parse_wait(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        return Some(seconds.ceil().max(0.0) as u64);
    }
    // Durations such as "6m0s" or "1.5s".
    if value.ends_with('s') && value.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let mut total = 0.0;
        let mut number = String::new();
        for c in value.chars() {
            if c.is_ascii_digit() || c == '.' {
                number.push(c);
                continue;
            }
            let unit = match c {
                'h' => 3600.0,
                'm' => 60.0,
                's' => 1.0,
                _ => return None,
            };
            total += std::mem::take(&mut number).parse::<f64>().ok()? * unit;
        }
        return Some(total.ceil() as u64);
    }
    None
}

/// Streams a chat completion, calling `on_text` with each text fragment as it arrives, and
/// returns the assembled text, tool calls, and usage.
pub async fn stream_chat(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    request: ChatRequest<'_>,
    mut on_text: impl FnMut(&str),
) -> Result<Completion, ModelError> {
    let url = format!(
        "{}/chat/completions",
        endpoint.base_url.trim_end_matches('/')
    );
    let mut body = json!({
        "model": request.model,
        "messages": request.messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(ToolSpec::to_wire).collect();
    }
    let mut builder = http.post(url).json(&body);
    if let Some(key) = &endpoint.api_key {
        builder = builder.bearer_auth(key);
    }
    let response = builder.send().await?;
    let status = response.status();
    if !status.is_success() {
        let retry_after_s = retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        return Err(ModelError::Status {
            status: status.as_u16(),
            body: body.chars().take(2000).collect(),
            retry_after_s,
        });
    }

    let mut assembler = Assembler::default();
    let mut lines = DataLines::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        for payload in lines.push(&chunk?) {
            if payload == "[DONE]" {
                return Ok(assembler.finish());
            }
            let chunk: StreamChunk = serde_json::from_str(&payload)
                .map_err(|error| ModelError::Decode(format!("{error} in {payload}")))?;
            if let Some(text) = assembler.apply(chunk) {
                on_text(&text);
            }
        }
    }
    Ok(assembler.finish())
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// Accumulates streamed deltas; tool calls arrive in fragments keyed by index.
#[derive(Debug, Default)]
struct Assembler {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
}

impl Assembler {
    /// Applies one chunk and returns its text fragment, if any.
    fn apply(&mut self, chunk: StreamChunk) -> Option<String> {
        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cached_tokens: usage
                    .prompt_tokens_details
                    .map_or(0, |details| details.cached_tokens),
            });
        }
        let delta = chunk.choices.into_iter().next()?.delta;
        for fragment in delta.tool_calls {
            while self.tool_calls.len() <= fragment.index {
                self.tool_calls.push(ToolCall {
                    id: String::new(),
                    kind: function_kind(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                });
            }
            let call = &mut self.tool_calls[fragment.index];
            if let Some(id) = fragment.id {
                call.id = id;
            }
            if let Some(function) = fragment.function {
                call.function
                    .name
                    .push_str(&function.name.unwrap_or_default());
                call.function
                    .arguments
                    .push_str(&function.arguments.unwrap_or_default());
            }
        }
        let text = delta.content.filter(|text| !text.is_empty())?;
        self.text.push_str(&text);
        Some(text)
    }

    fn finish(self) -> Completion {
        Completion {
            text: self.text,
            tool_calls: self.tool_calls,
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::post;

    async fn fake_endpoint(body: &'static str) -> Endpoint {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || async move { ([("content-type", "text/event-stream")], body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Endpoint {
            base_url,
            api_key: Some("k".into()),
        }
    }

    #[tokio::test]
    async fn text_tool_calls_and_usage_are_assembled_from_the_stream() {
        let endpoint = fake_endpoint(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n",
        ))
        .await;

        let mut streamed = String::new();
        let completion = stream_chat(
            &reqwest::Client::new(),
            &endpoint,
            ChatRequest {
                model: "m",
                messages: &[ChatMessage::User {
                    content: "hi".into(),
                }],
                tools: &[],
            },
            |text| streamed.push_str(text),
        )
        .await
        .unwrap();

        assert_eq!(streamed, "Hello");
        assert_eq!(completion.text, "Hello");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].id, "c1");
        assert_eq!(completion.tool_calls[0].function.name, "read_file");
        assert_eq!(
            completion.tool_calls[0].function.arguments,
            r#"{"path":"a"}"#
        );
        assert_eq!(
            completion.usage,
            Some(Usage {
                prompt_tokens: 12,
                completion_tokens: 5,
                cached_tokens: 4
            })
        );
    }

    #[tokio::test]
    async fn http_errors_carry_the_status_and_body() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "bad key") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let error = stream_chat(
            &reqwest::Client::new(),
            &Endpoint {
                base_url,
                api_key: None,
            },
            ChatRequest {
                model: "m",
                messages: &[],
                tools: &[],
            },
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, ModelError::Status { status: 401, ref body, .. } if body == "bad key"),
            "{error}"
        );
    }

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
    fn reset_hints_are_read_in_their_common_forms() {
        assert_eq!(parse_wait("120"), Some(120));
        assert_eq!(parse_wait("1.2"), Some(2));
        assert_eq!(parse_wait("6m0s"), Some(360));
        assert_eq!(parse_wait("1h2m3s"), Some(3723));
        assert_eq!(parse_wait("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        let limited = ModelError::Status {
            status: 429,
            body: String::new(),
            retry_after_s: Some(30),
        };
        assert!(limited.is_usage_limit());
        assert_eq!(limited.retry_after_s(), Some(30));
    }
}
