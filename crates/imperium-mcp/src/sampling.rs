//! Server-initiated sampling (`sampling/createMessage`): the server asks the client to have a
//! model continue a conversation. The request is checked and bounded here; the [`Sampler`]
//! behind it asks the user and runs the session's model.

use imperium_api::{Sampler, SamplingMessage, SamplingRequest};
use serde_json::{Value, json};

/// Most tokens a reply may generate when the server names none.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;
/// Most tokens a reply may generate whatever the server asks for.
pub const MAX_TOKENS: u32 = 4096;
/// Most characters of prompt (messages and system prompt together) one request may carry.
pub const MAX_PROMPT_CHARS: usize = 100_000;

/// JSON-RPC error code for a request the user declined (the MCP convention).
const DECLINED: i64 = -1;
const INVALID_PARAMS: i64 = -32602;

/// Where a sampling request is answered: the sampler, and which server is asking.
#[derive(Clone, Copy)]
pub struct Sampling<'a> {
    pub sampler: &'a dyn Sampler,
    pub server: &'a str,
}

/// Answers one `sampling/createMessage`: the result to send back, or a JSON-RPC error.
pub(crate) async fn create_message(
    sampling: Sampling<'_>,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let request = parse(sampling.server, params).map_err(|message| (INVALID_PARAMS, message))?;
    let reply = sampling
        .sampler
        .sample(request)
        .await
        .map_err(|message| (DECLINED, message))?;
    Ok(json!({
        "role": "assistant",
        "content": {"type": "text", "text": reply.text},
        "model": reply.model,
        "stopReason": if reply.truncated { "maxTokens" } else { "endTurn" },
    }))
}

/// Reads and bounds the request's parameters. Only text is supported.
fn parse(server: &str, params: &Value) -> Result<SamplingRequest, String> {
    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or("messages are required")?;
    let mut parsed = Vec::new();
    let mut chars = 0;
    for message in messages {
        let content = message.get("content").ok_or("a message has no content")?;
        let text = text_of(content)?;
        chars += text.chars().count();
        parsed.push(SamplingMessage {
            assistant: message.get("role").and_then(Value::as_str) == Some("assistant"),
            text,
        });
    }
    let system_prompt = params
        .get("systemPrompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.is_empty())
        .map(str::to_string);
    chars += system_prompt
        .as_ref()
        .map_or(0, |prompt| prompt.chars().count());
    if chars > MAX_PROMPT_CHARS {
        return Err(format!(
            "the request carries {chars} characters; at most {MAX_PROMPT_CHARS} are accepted"
        ));
    }
    let max_tokens = params
        .get("maxTokens")
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0)
        .map_or(DEFAULT_MAX_TOKENS, |tokens| {
            u32::try_from(tokens.min(u64::from(MAX_TOKENS))).unwrap_or(MAX_TOKENS)
        });
    Ok(SamplingRequest {
        server: server.to_string(),
        system_prompt,
        messages: parsed,
        max_tokens,
    })
}

/// The text of one content block, or of an array of them.
fn text_of(content: &Value) -> Result<String, String> {
    if let Some(blocks) = content.as_array() {
        let texts: Result<Vec<String>, String> = blocks.iter().map(text_of).collect();
        return Ok(texts?.join("\n"));
    }
    match content.get("type").and_then(Value::as_str) {
        Some("text") => Ok(content
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()),
        Some(other) => Err(format!(
            "{other} content is not supported; this client samples text only"
        )),
        None => Err("a message's content has no type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_read_and_bounded() {
        let request = parse(
            "s",
            &json!({
                "messages": [
                    {"role": "user", "content": {"type": "text", "text": "hi"}},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": "a"}, {"type": "text", "text": "b"}]},
                ],
                "systemPrompt": "be brief",
                "maxTokens": 999_999,
            }),
        )
        .unwrap();
        assert_eq!(request.max_tokens, MAX_TOKENS);
        assert_eq!(request.system_prompt.as_deref(), Some("be brief"));
        assert_eq!(request.messages.len(), 2);
        assert!(request.messages[1].assistant);
        assert_eq!(request.messages[1].text, "a\nb");
        let unset = parse(
            "s",
            &json!({"messages": [{"role": "user", "content": {"type": "text", "text": "x"}}]}),
        )
        .unwrap();
        assert_eq!(unset.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn unsupported_or_oversized_requests_are_refused() {
        assert!(parse("s", &json!({})).is_err());
        assert!(parse("s", &json!({"messages": []})).is_err());
        let image =
            json!({"messages": [{"role": "user", "content": {"type": "image", "data": "x"}}]});
        assert!(parse("s", &image).unwrap_err().contains("text only"));
        let big = "x".repeat(MAX_PROMPT_CHARS + 1);
        let long =
            json!({"messages": [{"role": "user", "content": {"type": "text", "text": big}}]});
        assert!(parse("s", &long).unwrap_err().contains("at most"));
    }
}
