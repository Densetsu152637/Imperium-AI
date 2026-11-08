//! Context breakdowns: what a model read on one call, by category, counted from the request
//! exactly as it was sent.
//!
//! The agent records which part of each message is what as it builds them (the system
//! prompt's parts, a user message's preamble), so the request divides into labelled segments
//! without guessing. Segments are then counted, best method first:
//!
//! - **exact**: each segment with the model's own tokenizer (llama.cpp's `/tokenize`), cached by
//!   content so unchanged messages are counted once; the chat template's overhead is the
//!   provider's reported prompt size minus the segments. Tool definitions count as the
//!   chat template renders them (`/apply-template`), since templates add their own tool
//!   instructions and re-serialise the JSON.
//! - **calibrated**: each segment estimated, then scaled so the categories add up to the
//!   reported prompt size.
//! - **estimated**: no usage was reported, so nothing anchors the total.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Mutex;
use std::time::Duration;

use imperium_models::{ChatMessage, Endpoint, ModelError, ToolSpec, Usage, llamacpp};
use imperium_protocol::session::{ContextCategory, SessionEvent, TokenCountMethod};

use crate::ResolvedModel;
use crate::session::lock;

/// The longest a breakdown may spend asking the model's server before it falls back to
/// calibrating, so a slow tokenizer cannot hold up the turn for long.
const TOKENIZE_BUDGET: Duration = Duration::from_secs(5);
/// Cached token counts kept before the cache starts over.
const CACHE_LIMIT: usize = 100_000;
/// What joins the labelled parts of one message.
pub(crate) const PART_SEPARATOR: &str = "\n\n";

/// The kinds of text a request carries, in the order breakdowns list them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Category {
    SystemBase,
    SystemSkills,
    SystemMemory,
    SystemCitations,
    SystemProject,
    ToolsBuiltin,
    ToolsMcp,
    Manifest,
    Memories,
    User,
    Assistant,
    ToolResults,
    Overhead,
}

impl Category {
    const ALL: [Category; 13] = [
        Category::SystemBase,
        Category::SystemSkills,
        Category::SystemMemory,
        Category::SystemCitations,
        Category::SystemProject,
        Category::ToolsBuiltin,
        Category::ToolsMcp,
        Category::Manifest,
        Category::Memories,
        Category::User,
        Category::Assistant,
        Category::ToolResults,
        Category::Overhead,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Category::SystemBase => "system_base",
            Category::SystemSkills => "system_skills",
            Category::SystemMemory => "system_memory",
            Category::SystemCitations => "system_citations",
            Category::SystemProject => "system_project",
            Category::ToolsBuiltin => "tools_builtin",
            Category::ToolsMcp => "tools_mcp",
            Category::Manifest => "manifest",
            Category::Memories => "memories",
            Category::User => "user",
            Category::Assistant => "assistant",
            Category::ToolResults => "tool_results",
            Category::Overhead => "overhead",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Category::SystemBase => "System prompt",
            Category::SystemSkills => "Skills listing",
            Category::SystemMemory => "Memory instructions",
            Category::SystemCitations => "Web citation rules",
            Category::SystemProject => "Project instructions",
            Category::ToolsBuiltin => "Built-in tools",
            Category::ToolsMcp => "MCP tools",
            Category::Manifest => "Workflow context manifest",
            Category::Memories => "Recalled memories",
            Category::User => "User messages",
            Category::Assistant => "Assistant messages",
            Category::ToolResults => "Tool results",
            Category::Overhead => "Chat template and formatting",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Where each labelled part sits in a message's content.
pub(crate) type Spans = Vec<(Category, Range<usize>)>;

/// Joins labelled parts into one message's content, recording where each part went.
pub(crate) fn join_parts(parts: Vec<(Category, String)>) -> (String, Spans) {
    let mut content = String::new();
    let mut spans = Vec::with_capacity(parts.len());
    for (category, text) in parts {
        if !content.is_empty() {
            content.push_str(PART_SEPARATOR);
        }
        let start = content.len();
        content.push_str(&text);
        spans.push((category, start..content.len()));
    }
    (content, spans)
}

/// The preamble of a user message the agent did not build in this process (a conversation
/// restored after a restart): the manifest and recalled memories it wrote are recognised by
/// the tags it wraps them in.
fn restored_spans(content: &str) -> Spans {
    let mut spans = Vec::new();
    let mut start = 0;
    for (category, open, close) in [
        (
            Category::Manifest,
            "<context-manifest",
            "</context-manifest>",
        ),
        (
            Category::Memories,
            "<relevant-memories>",
            "</relevant-memories>",
        ),
    ] {
        let rest = &content[start..];
        if rest.starts_with(open)
            && let Some(end) = rest.find(close)
        {
            let end = start + end + close.len();
            spans.push((category, start..end));
            start = (end + PART_SEPARATOR.len()).min(content.len());
        }
    }
    spans.push((Category::User, start..content.len()));
    spans
}

/// The tool definitions as sent, one JSON array per category (built-in tools, then MCP tools,
/// whose specs occupy `mcp` in `specs`).
pub(crate) fn tool_texts(specs: &[ToolSpec], mcp: Range<usize>) -> Vec<(Category, String)> {
    let wire = |category: Category, tools: Vec<&ToolSpec>| {
        let array: Vec<_> = tools.into_iter().map(ToolSpec::to_wire).collect();
        (!array.is_empty()).then(|| {
            let text = serde_json::to_string(&array).unwrap_or_default();
            (category, text)
        })
    };
    let (builtin, mcp_specs): (Vec<_>, Vec<_>) = specs
        .iter()
        .enumerate()
        .partition(|(index, _)| !mcp.contains(index));
    [
        wire(
            Category::ToolsBuiltin,
            builtin.into_iter().map(|(_, spec)| spec).collect(),
        ),
        wire(
            Category::ToolsMcp,
            mcp_specs.into_iter().map(|(_, spec)| spec).collect(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// One model request: its messages with the parts recorded for them, and its tool definitions.
pub(crate) struct Request<'a> {
    pub messages: &'a [ChatMessage],
    /// Labelled parts by message index; messages without an entry are one part.
    pub spans: &'a HashMap<usize, Spans>,
    pub tools: &'a [(Category, String)],
}

impl<'a> Request<'a> {
    /// The request's text, segment by segment. Together the segments are everything the
    /// request carries except formatting: roles, call ids, separators, and template markup.
    fn segments(&self) -> Vec<(Category, &'a str)> {
        let mut segments: Vec<(Category, &'a str)> = Vec::new();
        for (index, message) in self.messages.iter().enumerate() {
            let recorded = self.spans.get(&index);
            match message {
                ChatMessage::System { content } => match recorded {
                    Some(spans) => slices(content, spans, &mut segments),
                    None => segments.push((Category::SystemBase, content)),
                },
                ChatMessage::User { content } => match recorded {
                    Some(spans) => slices(content, spans, &mut segments),
                    None => slices(content, &restored_spans(content), &mut segments),
                },
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    if let Some(content) = content {
                        segments.push((Category::Assistant, content));
                    }
                    for call in tool_calls {
                        segments.push((Category::Assistant, &call.function.name));
                        segments.push((Category::Assistant, &call.function.arguments));
                    }
                }
                ChatMessage::Tool { content, .. } => {
                    segments.push((Category::ToolResults, content));
                }
            }
        }
        for (category, text) in self.tools {
            segments.push((*category, text));
        }
        segments.retain(|(_, text)| !text.is_empty());
        segments
    }

    /// A rough count of the formatting the chat template adds: role markers per message, a
    /// wrapper per tool call, the tool block, and the reply's opening.
    fn formatting_estimate(&self) -> u64 {
        let calls: usize = self
            .messages
            .iter()
            .map(|message| match message {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.len(),
                _ => 0,
            })
            .sum();
        let tools = if self.tools.is_empty() { 0 } else { 12 };
        (4 * self.messages.len() + 4 * calls + tools + 3) as u64
    }
}

fn slices<'a>(content: &'a str, spans: &Spans, out: &mut Vec<(Category, &'a str)>) {
    for (category, range) in spans {
        if let Some(text) = content.get(range.clone()) {
            out.push((*category, text));
        }
    }
}

/// A rough token count for text when no tokenizer is at hand, tuned for the mix of prose, code,
/// and JSON agents send: a word of up to six letters or digits is one token and longer ones one
/// more per six; punctuation counts one per two characters; a run of newlines or of indenting
/// spaces is one; any other character (such as CJK) is one.
pub(crate) fn estimate_tokens(text: &str) -> u64 {
    #[derive(PartialEq)]
    enum Run {
        None,
        Word,
        Punct,
        Newlines,
        Spaces,
    }
    let mut tokens = 0u64;
    let mut run = Run::None;
    let mut length = 0u64;
    let flush = |run: &Run, length: u64| match run {
        Run::Word => 1 + (length - 1) / 6,
        Run::Punct => length.div_ceil(2),
        Run::Newlines => 1,
        // One space joins the next word; longer runs (indentation) are a token.
        Run::Spaces => u64::from(length > 1),
        Run::None => 0,
    };
    for c in text.chars() {
        let kind = if c.is_ascii_alphanumeric() || c == '_' {
            Run::Word
        } else if c == '\n' || c == '\r' {
            Run::Newlines
        } else if c == ' ' || c == '\t' {
            Run::Spaces
        } else if c.is_ascii() {
            Run::Punct
        } else {
            tokens += flush(&run, length);
            tokens += 1;
            run = Run::None;
            length = 0;
            continue;
        };
        if kind != run {
            tokens += flush(&run, length);
            run = kind;
            length = 0;
        }
        length += 1;
    }
    tokens + flush(&run, length)
}

/// Scales `estimates` so they add up to `total` exactly, keeping their proportions: each gets
/// its floored share, and what is left goes to the largest remainders (earlier first on ties).
/// With nothing to scale, the last entry takes the total.
pub(crate) fn calibrate(estimates: &[u64], total: u64) -> Vec<u64> {
    let sum: u128 = estimates.iter().map(|&e| u128::from(e)).sum();
    if sum == 0 {
        let mut scaled = vec![0; estimates.len()];
        if let Some(last) = scaled.last_mut() {
            *last = total;
        }
        return scaled;
    }
    let shares: Vec<(u64, u128)> = estimates
        .iter()
        .map(|&estimate| {
            let exact = u128::from(estimate) * u128::from(total);
            ((exact / sum) as u64, exact % sum)
        })
        .collect();
    let mut scaled: Vec<u64> = shares.iter().map(|(floor, _)| *floor).collect();
    let left = total - scaled.iter().sum::<u64>();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|a, b| shares[*b].1.cmp(&shares[*a].1).then(a.cmp(b)));
    for index in order.into_iter().take(left as usize) {
        scaled[index] += 1;
    }
    scaled
}

/// Whether `base_url` points at this machine, where a llama.cpp server may answer `/tokenize`.
fn is_loopback(base_url: &str) -> bool {
    let after_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// What a breakdown knows about the call it describes.
pub(crate) struct Call<'a> {
    pub http: &'a reqwest::Client,
    /// Where the request went (for a local model, its server).
    pub endpoint: &'a Endpoint,
    pub model: &'a ResolvedModel,
    pub usage: Option<Usage>,
}

/// A tool set's (built-in, MCP) tokens as a template renders them, or `None` when the server
/// cannot render templates.
type RenderedTools = Option<(u64, u64)>;

/// Counts shared by a session's agents: token counts by tokenizer and content, and what each
/// server reported about itself.
#[derive(Debug, Default)]
pub(crate) struct TokenCounter {
    counts: Mutex<HashMap<(String, u64, usize), u64>>,
    /// Context windows by server root; `None` when a server answered without one.
    windows: Mutex<HashMap<String, Option<u64>>>,
    /// Servers that refused a tokenize request (not llama.cpp), so they are not asked again.
    no_tokenizer: Mutex<HashSet<String>>,
    /// The tool block as each model's template renders it, by tokenizer and tool set:
    /// (built-in, MCP) tokens, or `None` when the server cannot render templates.
    rendered_tools: Mutex<HashMap<(String, u64), RenderedTools>>,
}

impl TokenCounter {
    /// The breakdown of `request` as sent on `call`. Never fails: when tokenizing fails or
    /// takes too long, it falls back to calibrating against the reported total.
    pub(crate) async fn breakdown(&self, call: Call<'_>, request: &Request<'_>) -> SessionEvent {
        let segments = request.segments();
        let reported = call.usage.filter(|usage| usage.prompt_tokens > 0);
        let root = llamacpp::server_root(&call.endpoint.base_url).to_string();
        // A server on this machine may be llama.cpp, with a tokenizer and properties to ask.
        let on_machine = call.model.local || is_loopback(&call.endpoint.base_url);
        let tokenizer = on_machine && !lock(&self.no_tokenizer).contains(&root);
        let mut exact = None;
        let mut window = None;
        let mut rendered_tools = None;
        if on_machine {
            let work = async {
                let counted = if tokenizer {
                    Some(self.count_all(&call, &call.model.spec, &segments).await)
                } else {
                    None
                };
                let rendered = match &counted {
                    Some(Ok(_)) => self.rendered_tools(&call, request.tools).await,
                    _ => None,
                };
                (counted, rendered, self.window(&call, &root).await)
            };
            match tokio::time::timeout(TOKENIZE_BUDGET, work).await {
                Ok((counted, rendered, server_window)) => {
                    window = server_window;
                    rendered_tools = rendered;
                    match counted {
                        None => {}
                        Some(Ok(counts)) => exact = Some(counts),
                        Some(Err(error)) => {
                            tracing::debug!(model = %call.model.spec, %error, "tokenizer unavailable; calibrating");
                            if matches!(
                                error,
                                ModelError::Status {
                                    status: 400..=499,
                                    ..
                                }
                            ) {
                                lock(&self.no_tokenizer).insert(root.clone());
                            }
                        }
                    }
                }
                Err(_) => {
                    tracing::debug!(model = %call.model.spec, "tokenizing took too long; calibrating");
                }
            }
        }
        let mut totals = [0u64; Category::ALL.len()];
        match &exact {
            Some(counts) => {
                for ((category, _), count) in segments.iter().zip(counts) {
                    totals[category.index()] += count;
                }
                // The tools as the template renders them, not as the JSON sent.
                if let Some((builtin, mcp)) = rendered_tools {
                    totals[Category::ToolsBuiltin.index()] = builtin;
                    totals[Category::ToolsMcp.index()] = mcp;
                }
            }
            None => {
                for (category, text) in &segments {
                    totals[category.index()] += estimate_tokens(text);
                }
            }
        }
        let overhead = Category::Overhead.index();
        let (method, overhead_tokens) = match (reported, &exact) {
            (Some(usage), Some(_)) => {
                let parts: u64 = totals.iter().sum();
                let overhead = i64::try_from(usage.prompt_tokens).unwrap_or(i64::MAX)
                    - i64::try_from(parts).unwrap_or(i64::MAX);
                (TokenCountMethod::Exact, overhead)
            }
            (Some(usage), None) => {
                totals[overhead] = request.formatting_estimate();
                let scaled = calibrate(&totals, usage.prompt_tokens);
                totals.copy_from_slice(&scaled);
                let overhead = i64::try_from(totals[overhead]).unwrap_or(i64::MAX);
                (TokenCountMethod::Calibrated, overhead)
            }
            (None, _) => {
                let estimate = request.formatting_estimate();
                (
                    TokenCountMethod::Estimated,
                    i64::try_from(estimate).unwrap_or(i64::MAX),
                )
            }
        };
        let categories = Category::ALL
            .iter()
            .filter_map(|&category| {
                let tokens = if category == Category::Overhead {
                    overhead_tokens
                } else {
                    i64::try_from(totals[category.index()]).unwrap_or(i64::MAX)
                };
                (tokens != 0).then(|| ContextCategory {
                    key: category.key().into(),
                    label: category.label().into(),
                    tokens,
                })
            })
            .collect();
        SessionEvent::ContextBreakdown {
            model: call.model.spec.clone(),
            context_window: window.or(call.model.context_window),
            method,
            categories,
            prompt_tokens: reported.map(|usage| usage.prompt_tokens),
            cached_tokens: call.usage.map_or(0, |usage| usage.cached_tokens),
        }
    }

    /// Every segment's count from the tokenizer, asking the server only for text it has not
    /// counted before.
    async fn count_all(
        &self,
        call: &Call<'_>,
        tokenizer: &str,
        segments: &[(Category, &str)],
    ) -> Result<Vec<u64>, ModelError> {
        let mut counts = Vec::with_capacity(segments.len());
        for (_, text) in segments {
            let key = (tokenizer.to_string(), content_hash(text), text.len());
            let cached = lock(&self.counts).get(&key).copied();
            let count = match cached {
                Some(count) => count,
                None => {
                    let count = llamacpp::count_tokens(call.http, call.endpoint, text).await?;
                    let mut cache = lock(&self.counts);
                    if cache.len() >= CACHE_LIMIT {
                        cache.clear();
                    }
                    cache.insert(key, count);
                    count
                }
            };
            counts.push(count);
        }
        Ok(counts)
    }

    /// The tokens the model's chat template spends on `tools` (built-in, MCP): the difference
    /// it makes to rendering a minimal conversation, so the template's tool instructions and
    /// its own JSON spacing count as tools. Measured once per tool set; `None` when the server
    /// has no `/apply-template`, leaving the JSON as sent.
    async fn rendered_tools(
        &self,
        call: &Call<'_>,
        tools: &[(Category, String)],
    ) -> Option<(u64, u64)> {
        if tools.is_empty() {
            return None;
        }
        let joined: String = tools.iter().map(|(_, text)| text.as_str()).collect();
        let key = (call.model.spec.clone(), content_hash(&joined));
        if let Some(known) = lock(&self.rendered_tools).get(&key) {
            return *known;
        }
        let array = |category: Category| -> Vec<serde_json::Value> {
            tools
                .iter()
                .filter(|(c, _)| *c == category)
                .filter_map(|(_, text)| serde_json::from_str::<Vec<serde_json::Value>>(text).ok())
                .flatten()
                .collect()
        };
        let builtin = array(Category::ToolsBuiltin);
        let all: Vec<_> = builtin
            .iter()
            .cloned()
            .chain(array(Category::ToolsMcp))
            .collect();
        let probe = serde_json::json!([
            {"role": "system", "content": "x"},
            {"role": "user", "content": "x"},
        ]);
        let (http, endpoint) = (call.http, call.endpoint);
        let measured = async {
            let bare = llamacpp::template_tokens(http, endpoint, &probe, &[]).await?;
            let with_builtin = llamacpp::template_tokens(http, endpoint, &probe, &builtin).await?;
            let with_all = llamacpp::template_tokens(http, endpoint, &probe, &all).await?;
            Ok::<_, ModelError>((
                with_builtin.saturating_sub(bare),
                with_all.saturating_sub(with_builtin),
            ))
        }
        .await;
        let rendered = match measured {
            Ok(counts) => Some(counts),
            // A server without template rendering keeps the JSON as sent; a dropped
            // connection is asked again next time.
            Err(ModelError::Status { .. } | ModelError::Decode(_)) => None,
            Err(error) => {
                tracing::debug!(model = %call.model.spec, %error, "could not render the tool block");
                return None;
            }
        };
        lock(&self.rendered_tools).insert(key, rendered);
        rendered
    }

    /// The server's context window, asked once per server. A failed connection is asked again
    /// next time; a server that answers without one is not.
    async fn window(&self, call: &Call<'_>, root: &str) -> Option<u64> {
        if let Some(window) = lock(&self.windows).get(root) {
            return *window;
        }
        let window = match llamacpp::context_window(call.http, call.endpoint).await {
            Ok(window) => window,
            Err(ModelError::Status { .. } | ModelError::Decode(_)) => None,
            Err(_) => return None,
        };
        lock(&self.windows).insert(root.to_string(), window);
        window
    }
}

fn content_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_models::{FunctionCall, ToolCall};
    use serde_json::json;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("the {name} tool"),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn joined_parts_record_where_each_part_went() {
        let (content, spans) = join_parts(vec![
            (
                Category::Manifest,
                "<context-manifest role=\"root\">x</context-manifest>".into(),
            ),
            (Category::User, "fix the bug".into()),
        ]);
        assert_eq!(
            &content[spans[0].1.clone()],
            "<context-manifest role=\"root\">x</context-manifest>"
        );
        assert_eq!(&content[spans[1].1.clone()], "fix the bug");
        assert_eq!(spans[1].0, Category::User);
    }

    #[test]
    fn segments_partition_the_request_by_category() {
        let (system, system_spans) = join_parts(vec![
            (Category::SystemBase, "You are Imperium.".into()),
            (Category::SystemProject, "Use tabs.".into()),
        ]);
        let (user, user_spans) = join_parts(vec![
            (
                Category::Memories,
                "<relevant-memories>m</relevant-memories>".into(),
            ),
            (Category::User, "read a.txt".into()),
        ]);
        let messages = vec![
            ChatMessage::System { content: system },
            ChatMessage::User { content: user },
            ChatMessage::Assistant {
                content: Some("Reading.".into()),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"a.txt"}"#.into(),
                    },
                }],
            },
            ChatMessage::Tool {
                tool_call_id: "c1".into(),
                content: "hello".into(),
            },
        ];
        let spans = HashMap::from([(0, system_spans), (1, user_spans)]);
        let tools = tool_texts(&[spec("read_file"), spec("mcp__s__t")], 1..2);
        let request = Request {
            messages: &messages,
            spans: &spans,
            tools: &tools,
        };
        let segments = request.segments();
        let text_of = |category| -> Vec<&str> {
            segments
                .iter()
                .filter(|(c, _)| *c == category)
                .map(|(_, text)| *text)
                .collect()
        };
        assert_eq!(text_of(Category::SystemBase), ["You are Imperium."]);
        assert_eq!(text_of(Category::SystemProject), ["Use tabs."]);
        assert_eq!(
            text_of(Category::Memories),
            ["<relevant-memories>m</relevant-memories>"]
        );
        assert_eq!(text_of(Category::User), ["read a.txt"]);
        assert_eq!(
            text_of(Category::Assistant),
            ["Reading.", "read_file", r#"{"path":"a.txt"}"#]
        );
        assert_eq!(text_of(Category::ToolResults), ["hello"]);
        // Each tool category is the JSON array exactly as the request sends it.
        let builtin: serde_json::Value =
            serde_json::from_str(text_of(Category::ToolsBuiltin)[0]).unwrap();
        assert_eq!(builtin, json!([spec("read_file").to_wire()]));
        let mcp: serde_json::Value = serde_json::from_str(text_of(Category::ToolsMcp)[0]).unwrap();
        assert_eq!(mcp, json!([spec("mcp__s__t").to_wire()]));
    }

    #[test]
    fn restored_user_messages_split_at_the_preamble_tags() {
        let content = "<context-manifest role=\"root\">\n- a\n</context-manifest>\n\n\
                       <relevant-memories>\nm\n</relevant-memories>\n\nhello";
        let spans = restored_spans(content);
        let parts: Vec<_> = spans
            .iter()
            .map(|(category, range)| (*category, &content[range.clone()]))
            .collect();
        assert_eq!(parts[0].0, Category::Manifest);
        assert!(parts[0].1.ends_with("</context-manifest>"));
        assert_eq!(parts[1].0, Category::Memories);
        assert_eq!(parts[2], (Category::User, "hello"));
        assert_eq!(restored_spans("just text"), vec![(Category::User, 0..9)]);
    }

    #[test]
    fn calibration_keeps_proportions_and_hits_the_total() {
        assert_eq!(calibrate(&[10, 30, 60], 200), vec![20, 60, 120]);
        let scaled = calibrate(&[1, 1, 1], 100);
        assert_eq!(scaled.iter().sum::<u64>(), 100);
        assert_eq!(scaled, vec![34, 33, 33]);
        let scaled = calibrate(&[7, 0, 13, 5], 997);
        assert_eq!(scaled.iter().sum::<u64>(), 997);
        assert_eq!(scaled[1], 0);
        // Nothing to scale: the last entry (the overhead) takes it all.
        assert_eq!(calibrate(&[0, 0], 5), vec![0, 5]);
    }

    #[test]
    fn estimates_follow_the_shape_of_the_text() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("read the file"), 3);
        // A long identifier costs more than a short word; punctuation pairs are one token.
        assert_eq!(estimate_tokens("internationalisation"), 4);
        assert_eq!(estimate_tokens(r#"{"a":1}"#), 5);
        assert_eq!(estimate_tokens("a\n\n    b"), 4);
        assert_eq!(estimate_tokens("日本"), 2);
    }

    #[test]
    fn only_this_machine_counts_as_loopback() {
        assert!(is_loopback("http://127.0.0.1:8080/v1"));
        assert!(is_loopback("http://localhost:1234/v1"));
        assert!(is_loopback("http://[::1]:8080/v1"));
        assert!(!is_loopback("https://openrouter.ai/api/v1"));
        assert!(!is_loopback("http://192.168.1.4:8080/v1"));
    }
}
