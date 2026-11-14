//! What the user is shown when an MCP server asks the session's model for a reply: enough to
//! judge the request (who, how big, how costly, what it says) without the text being able to
//! pass for anything but a quotation.

use imperium_api::{Pricing, SamplingRequest, Usage, truncate_chars};

/// Characters per token, for estimates.
const CHARS_PER_TOKEN: u64 = 4;
const EXCERPT: usize = 200;

/// Tokens the request's prompt is estimated to take.
pub(crate) fn prompt_tokens(request: &SamplingRequest) -> u64 {
    prompt_chars(request).div_ceil(CHARS_PER_TOKEN)
}

fn prompt_chars(request: &SamplingRequest) -> u64 {
    let system = request
        .system_prompt
        .as_ref()
        .map_or(0, |prompt| prompt.chars().count());
    let messages: usize = request
        .messages
        .iter()
        .map(|message| message.text.chars().count())
        .sum();
    (system + messages) as u64
}

/// `text` cut to `limit` characters, quoted, with line breaks shown as `⏎` and quotes escaped.
pub(crate) fn quote(text: &str, limit: usize) -> String {
    let flat: String = truncate_chars(text, limit)
        .chars()
        .flat_map(|c| match c {
            '\n' | '\r' => vec!['⏎'],
            '"' => vec!['\\', '"'],
            c if c.is_control() => vec![' '],
            c => vec![c],
        })
        .collect();
    format!("\"{flat}\"")
}

/// The most the call can cost with `pricing`: the prompt as estimated plus every token the
/// reply may generate, all at full price.
pub(crate) fn cost_ceiling(pricing: Option<&Pricing>, request: &SamplingRequest) -> Option<f64> {
    pricing.map(|pricing| {
        pricing.cost_usd(&Usage {
            prompt_tokens: prompt_tokens(request),
            completion_tokens: u64::from(request.max_tokens),
            cached_tokens: 0,
        })
    })
}

/// The permission request's text.
pub(crate) fn summary(model: &str, pricing: Option<&Pricing>, request: &SamplingRequest) -> String {
    let cost = match cost_ceiling(pricing, request) {
        Some(usd) => format!("at most about ${usd:.4}"),
        None => "cost unknown (the model has no price)".to_string(),
    };
    let system = match &request.system_prompt {
        Some(prompt) => format!("a system prompt {}", quote(prompt, EXCERPT)),
        None => "no system prompt".to_string(),
    };
    let last = request
        .messages
        .last()
        .map_or(String::new(), |message| quote(&message.text, EXCERPT));
    format!(
        "MCP server {} asks {model} to write a reply of up to {} tokens ({cost}). The request \
         has {} message(s), {} characters (about {} tokens), and {system}. Its last message \
         is {last}. Everything quoted comes from the server.",
        request.server,
        request.max_tokens,
        request.messages.len(),
        prompt_chars(request),
        prompt_tokens(request),
    )
}

#[cfg(test)]
mod tests {
    use imperium_api::SamplingMessage;

    use super::*;

    fn request() -> SamplingRequest {
        SamplingRequest {
            server: "w".into(),
            system_prompt: Some("be \"short\"\nand kind".into()),
            messages: vec![
                SamplingMessage {
                    assistant: false,
                    text: "first".into(),
                },
                SamplingMessage {
                    assistant: false,
                    text: "line one\nline two".into(),
                },
            ],
            max_tokens: 1000,
        }
    }

    #[test]
    fn the_summary_shows_size_cost_system_prompt_and_quotes_untrusted_text() {
        let pricing = Pricing {
            input_usd_per_mtok: 1.0,
            output_usd_per_mtok: 10.0,
            cached_input_usd_per_mtok: None,
        };
        let text = summary("fake:model", Some(&pricing), &request());
        assert!(text.contains("MCP server w asks fake:model"), "{text}");
        assert!(text.contains("up to 1000 tokens"), "{text}");
        assert!(text.contains("2 message(s)"), "{text}");
        assert!(
            text.contains("a system prompt \"be \\\"short\\\"⏎and kind\""),
            "{text}"
        );
        assert!(
            text.contains("last message is \"line one⏎line two\""),
            "{text}"
        );
        assert!(text.contains("at most about $0.0100"), "{text}");
        assert!(!text.contains('\n'));
        let unpriced = summary("fake:model", None, &request());
        assert!(unpriced.contains("cost unknown"), "{unpriced}");
        let mut bare = request();
        bare.system_prompt = None;
        assert!(summary("m", None, &bare).contains("no system prompt"));
    }

    #[test]
    fn long_text_is_cut() {
        let long = quote(&"x".repeat(1000), EXCERPT);
        assert!(long.chars().count() <= EXCERPT + 3);
    }
}
