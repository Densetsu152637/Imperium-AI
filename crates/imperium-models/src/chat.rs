use std::fmt;
use std::time::Instant;

use futures_util::StreamExt;
use imperium_api::{ChatMessage, Effort, FunctionCall, ToolCall, ToolSpec, Usage};
use serde::Deserialize;
use serde_json::json;

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

#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub tools: &'a [ToolSpec],
    /// For a llama.cpp server: the slot to run the request on (`id_slot`), with the slot's
    /// cached prompt kept and reused (`cache_prompt`). `None` leaves both to the server, and
    /// other endpoints never receive these fields.
    pub slot: Option<u32>,
    /// Most tokens the reply may generate (`max_tokens`); `None` sends none.
    pub max_tokens: Option<u32>,
    /// How hard a model that can reason thinks, sent in the endpoint's own form (see
    /// [`ReasoningStyle`]); `None` for models that cannot reason, which are sent nothing.
    pub effort: Option<Effort>,
}

/// How an endpoint takes a reasoning effort, decided by who serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningStyle {
    /// `reasoning_effort`: `low`, `medium` or `high` (`max` is sent as `high`, the highest
    /// level every server accepts). OpenAI's Chat Completions, llama.cpp (whose chat template
    /// receives the value; there is no per-request thinking budget, only the server's
    /// `--reasoning-budget`), vLLM, Ollama, and most other OpenAI-compatible services.
    ReasoningEffort,
    /// OpenRouter's `reasoning: {"effort": ...}`, which takes `max` as it is.
    OpenRouter,
    /// Anthropic's OpenAI-compatible endpoint, which ignores `reasoning_effort`: extended
    /// thinking with a token budget, `thinking: {"type": "enabled", "budget_tokens": N}`, at
    /// least 1024 and below `max_tokens`. Claude 5 models think adaptively whatever the budget.
    AnthropicThinking,
    /// No effort control is known (DeepSeek's and Mistral's APIs), so nothing is sent.
    Unsupported,
}

/// Thinking budget sent to Anthropic for each effort, in tokens.
const ANTHROPIC_BUDGETS: [(Effort, u32); 4] = [
    (Effort::Low, 1024),
    (Effort::Medium, 4096),
    (Effort::High, 12_288),
    (Effort::Max, 24_000),
];
/// Smallest thinking budget Anthropic accepts.
const ANTHROPIC_MIN_BUDGET: u32 = 1024;
/// Room for the answer after the thinking budget when the request sets no `max_tokens` (the
/// budget must stay below it); with the largest budget it totals 32,000, the smallest output
/// limit among the Claude models that take a budget.
const ANTHROPIC_REPLY_ROOM: u32 = 8000;

impl ReasoningStyle {
    /// The style for an endpoint, from its base URL's host.
    pub fn for_base_url(base_url: &str) -> Self {
        let after_scheme = base_url
            .split_once("://")
            .map_or(base_url, |(_, rest)| rest);
        let host = after_scheme
            .split(['/', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        if on("anthropic.com") {
            ReasoningStyle::AnthropicThinking
        } else if on("openrouter.ai") {
            ReasoningStyle::OpenRouter
        } else if on("deepseek.com") || on("mistral.ai") {
            ReasoningStyle::Unsupported
        } else {
            ReasoningStyle::ReasoningEffort
        }
    }

    /// Adds `effort` to a Chat Completions request body in this style.
    pub fn apply(self, body: &mut serde_json::Value, effort: Effort) {
        match self {
            ReasoningStyle::ReasoningEffort => {
                let level = match effort {
                    Effort::Max => Effort::High,
                    level => level,
                };
                body["reasoning_effort"] = json!(level.as_str());
            }
            ReasoningStyle::OpenRouter => {
                body["reasoning"] = json!({ "effort": effort.as_str() });
            }
            ReasoningStyle::AnthropicThinking => {
                let budget = ANTHROPIC_BUDGETS
                    .iter()
                    .find(|(level, _)| *level == effort)
                    .map_or(ANTHROPIC_MIN_BUDGET, |(_, budget)| *budget);
                let cap = body.get("max_tokens").and_then(serde_json::Value::as_u64);
                let budget = match cap {
                    Some(cap) => {
                        budget.min(u32::try_from(cap).unwrap_or(u32::MAX).saturating_sub(1))
                    }
                    None => {
                        body["max_tokens"] = json!(budget + ANTHROPIC_REPLY_ROOM);
                        budget
                    }
                };
                // A cap too small for the least budget leaves thinking off.
                if budget >= ANTHROPIC_MIN_BUDGET {
                    body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
                }
            }
            ReasoningStyle::Unsupported => {}
        }
    }
}

/// The assembled result of one streamed completion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// Absent when the endpoint does not report usage for streamed responses.
    pub usage: Option<Usage>,
    /// When the first generated text arrived, answer or reasoning. A thinking model streams
    /// its reasoning before the answer and counts it in `completion_tokens`, so generation
    /// speed is measured from here; the reasoning itself is not kept.
    pub first_token: Option<Instant>,
    /// Why the reply ended, from the stream's last choice: `stop`, `tool_calls`, or `length`
    /// when it hit `max_tokens`. Absent when the endpoint does not say.
    pub finish_reason: Option<String>,
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
    let body = request_body(request, ReasoningStyle::for_base_url(&endpoint.base_url));
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

/// The JSON body of a streamed chat completion request.
fn request_body(request: ChatRequest<'_>, style: ReasoningStyle) -> serde_json::Value {
    let mut body = json!({
        "model": request.model,
        "messages": request.messages.iter().map(ChatMessage::to_wire).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(ToolSpec::to_wire).collect();
    }
    if let Some(slot) = request.slot {
        // llama.cpp's chat endpoint passes these on to its completion task. A pinned slot that
        // holds another prompt still has it saved to the host cache and the agent's own prompt
        // restored, but only under the server's default `--slot-prompt-similarity` (never 0);
        // see `LlamaCpp::launch_args`.
        body["id_slot"] = json!(slot);
        body["cache_prompt"] = json!(true);
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(effort) = request.effort {
        style.apply(&mut body, effort);
    }
    body
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
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    /// A thinking model's reasoning: llama.cpp sends `reasoning_content` (b12180,
    /// `server_chat_msg_diff_to_json_oaicompat`); some OpenAI-compatible services `reasoning`.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
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

/// The `type` of every tool call the Chat Completions API returns.
fn function_kind() -> String {
    "function".to_string()
}

/// Accumulates streamed deltas; tool calls arrive in fragments keyed by index.
#[derive(Debug, Default)]
struct Assembler {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
    first_token: Option<Instant>,
    finish_reason: Option<String>,
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
        let choice = chunk.choices.into_iter().next()?;
        if choice.finish_reason.is_some() {
            self.finish_reason = choice.finish_reason;
        }
        let delta = choice.delta;
        let generated =
            |text: &Option<String>| text.as_deref().is_some_and(|text| !text.is_empty());
        if generated(&delta.content)
            || generated(&delta.reasoning_content)
            || generated(&delta.reasoning)
        {
            self.first_token.get_or_insert_with(Instant::now);
        }
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
            first_token: self.first_token,
            finish_reason: self.finish_reason,
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
                messages: &[ChatMessage::user("hi")],
                tools: &[],
                slot: None,
                max_tokens: None,
                effort: None,
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
    async fn reasoning_marks_the_first_token_without_becoming_reply_text() {
        let endpoint = fake_endpoint(concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}

",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\" more\"}}]}

",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":40}}

",
            "data: [DONE]

",
        ))
        .await;

        let mut streamed = String::new();
        let before = Instant::now();
        let completion = stream_chat(
            &reqwest::Client::new(),
            &endpoint,
            ChatRequest {
                model: "m",
                messages: &[],
                tools: &[],
                slot: None,
                max_tokens: None,
                effort: None,
            },
            |text| streamed.push_str(text),
        )
        .await
        .unwrap();

        assert!(streamed.is_empty() && completion.text.is_empty());
        assert!(completion.first_token.is_some_and(|first| first >= before));
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
                slot: None,
                max_tokens: None,
                effort: None,
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
    fn a_reply_cap_is_sent_only_when_set() {
        let request = |max_tokens| ChatRequest {
            model: "m",
            messages: &[],
            tools: &[],
            slot: None,
            max_tokens,
            effort: None,
        };
        let plain = ReasoningStyle::ReasoningEffort;
        assert!(
            request_body(request(None), plain)
                .get("max_tokens")
                .is_none()
        );
        assert_eq!(request_body(request(Some(512)), plain)["max_tokens"], 512);
    }

    #[test]
    fn llama_cpp_slots_are_requested_only_when_the_call_has_one() {
        let request = |slot| ChatRequest {
            model: "m",
            messages: &[],
            tools: &[],
            slot,
            max_tokens: None,
            effort: None,
        };
        let style = ReasoningStyle::ReasoningEffort;
        let plain = request_body(request(None), style);
        assert!(plain.get("id_slot").is_none() && plain.get("cache_prompt").is_none());
        let pinned = request_body(request(Some(1)), style);
        assert_eq!(pinned["id_slot"], 1);
        assert_eq!(pinned["cache_prompt"], true);
    }

    #[test]
    fn endpoints_get_the_reasoning_style_their_host_speaks() {
        let style = ReasoningStyle::for_base_url;
        assert_eq!(
            style("https://api.anthropic.com/v1/"),
            ReasoningStyle::AnthropicThinking
        );
        assert_eq!(
            style("https://openrouter.ai/api/v1"),
            ReasoningStyle::OpenRouter
        );
        assert_eq!(
            style("https://api.openai.com/v1"),
            ReasoningStyle::ReasoningEffort
        );
        assert_eq!(
            style("http://127.0.0.1:8080/v1"),
            ReasoningStyle::ReasoningEffort
        );
        assert_eq!(
            style("https://api.deepseek.com"),
            ReasoningStyle::Unsupported
        );
        assert_eq!(
            style("https://api.mistral.ai/v1"),
            ReasoningStyle::Unsupported
        );
        // A look-alike host is not the provider.
        assert_eq!(
            style("https://anthropic.com.example/v1"),
            ReasoningStyle::ReasoningEffort
        );
    }

    fn body_with(effort: Option<Effort>, max_tokens: Option<u32>, url: &str) -> serde_json::Value {
        request_body(
            ChatRequest {
                model: "m",
                messages: &[],
                tools: &[],
                slot: None,
                max_tokens,
                effort,
            },
            ReasoningStyle::for_base_url(url),
        )
    }

    #[test]
    fn models_that_cannot_reason_are_sent_no_reasoning_control() {
        for url in [
            "https://api.openai.com/v1",
            "https://openrouter.ai/api/v1",
            "https://api.anthropic.com/v1",
        ] {
            let body = body_with(None, None, url);
            for key in ["reasoning_effort", "reasoning", "thinking", "max_tokens"] {
                assert!(body.get(key).is_none(), "{url}: {key}");
            }
        }
        let body = body_with(Some(Effort::High), None, "https://api.deepseek.com");
        assert!(body.get("reasoning_effort").is_none() && body.get("thinking").is_none());
    }

    #[test]
    fn openai_style_servers_get_reasoning_effort_with_max_as_high() {
        for (effort, sent) in [
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::Max, "high"),
        ] {
            for url in ["https://api.openai.com/v1", "http://127.0.0.1:8080/v1"] {
                let body = body_with(Some(effort), None, url);
                assert_eq!(body["reasoning_effort"], sent);
                assert!(body.get("reasoning").is_none() && body.get("thinking").is_none());
            }
        }
    }

    #[test]
    fn openrouter_gets_a_reasoning_object_with_every_level() {
        for effort in Effort::ALL {
            let body = body_with(Some(effort), None, "https://openrouter.ai/api/v1");
            assert_eq!(body["reasoning"], json!({ "effort": effort.as_str() }));
            assert!(body.get("reasoning_effort").is_none());
        }
    }

    #[test]
    fn anthropic_gets_a_thinking_budget_below_max_tokens() {
        let anthropic = "https://api.anthropic.com/v1/";
        let mut last = 0;
        for effort in Effort::ALL {
            let body = body_with(Some(effort), None, anthropic);
            let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
            let cap = body["max_tokens"].as_u64().unwrap();
            assert_eq!(body["thinking"]["type"], "enabled");
            assert!(budget >= 1024 && budget > last, "{effort}");
            assert!(cap > budget && cap <= 32_000, "{effort}");
            assert!(body.get("reasoning_effort").is_none());
            last = budget;
        }
        // A caller's cap is kept and the budget fits under it.
        let capped = body_with(Some(Effort::Max), Some(5000), anthropic);
        assert_eq!(capped["max_tokens"], 5000);
        assert_eq!(capped["thinking"]["budget_tokens"], 4999);
        // Too small a cap for any budget leaves thinking off.
        let tiny = body_with(Some(Effort::Low), Some(600), anthropic);
        assert!(tiny.get("thinking").is_none());
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
