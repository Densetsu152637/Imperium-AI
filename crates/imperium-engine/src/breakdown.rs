//! Context breakdowns: what a model read on one call, by category, counted from the request
//! exactly as it was sent.
//!
//! The agent records which part of each message is what as it builds them (the system
//! prompt's parts, a user message's preamble), so the request divides into labelled segments
//! without guessing. Segments are then counted, best method first:
//!
//! - **exact**: each segment with the model's own tokenizer (when the model's client offers
//!   one, such as llama.cpp's `/tokenize`), cached by
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

use imperium_models::{ChatClient, ChatMessage, ModelError, Tokenizer, ToolSpec, Usage};
use imperium_protocol::session::{ContextCategory, SessionEvent, TokenCountMethod};

use crate::ResolvedModel;
use crate::session::lock;

/// The longest a breakdown may spend asking the model's server before it falls back to
/// calibrating, so a slow tokenizer cannot hold up the turn for long.
const TOKENIZE_BUDGET: Duration = Duration::from_secs(5);
/// Cached token counts kept before the cache starts over.
const CACHE_LIMIT: usize = 100_000;
/// What one attached image is counted as: providers charge by size and detail (OpenAI's
/// high-detail 1024x1024 tile set is 765, llama.cpp vision projectors a few hundred to a
/// thousand), and the image is not decoded here, so this is a middling estimate that the
/// reported prompt size calibrates when there is one.
pub(crate) const IMAGE_TOKENS: u64 = 800;
/// What joins the labelled parts of one message.
pub(crate) const PART_SEPARATOR: &str = "\n\n";
/// The tool whose results are skill instructions.
pub(crate) const LOAD_SKILL: &str = "load_skill";

/// The kinds of text a request carries, in the order breakdowns list them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Category {
    SystemBase,
    /// The index of skills and plugins loaded on use.
    SystemSkills,
    /// Skill instructions: eager ones in the system prompt, and `load_skill` results.
    SkillsLoaded,
    SystemMemory,
    SystemCitations,
    SystemProject,
    ToolsBuiltin,
    ToolsMcp,
    /// Tools of activated plugins.
    ToolsPlugins,
    Summary,
    Manifest,
    Memories,
    Attachments,
    User,
    Assistant,
    ToolResults,
    Overhead,
}

impl Category {
    const ALL: [Category; 17] = [
        Category::SystemBase,
        Category::SystemSkills,
        Category::SkillsLoaded,
        Category::SystemMemory,
        Category::SystemCitations,
        Category::SystemProject,
        Category::ToolsBuiltin,
        Category::ToolsMcp,
        Category::ToolsPlugins,
        Category::Summary,
        Category::Manifest,
        Category::Memories,
        Category::Attachments,
        Category::User,
        Category::Assistant,
        Category::ToolResults,
        Category::Overhead,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Category::SystemBase => "system_base",
            Category::SystemSkills => "system_skills",
            Category::SkillsLoaded => "skills_loaded",
            Category::SystemMemory => "system_memory",
            Category::SystemCitations => "system_citations",
            Category::SystemProject => "system_project",
            Category::ToolsBuiltin => "tools_builtin",
            Category::ToolsMcp => "tools_mcp",
            Category::ToolsPlugins => "tools_plugins",
            Category::Summary => "summary",
            Category::Manifest => "manifest",
            Category::Memories => "memories",
            Category::Attachments => "attachments",
            Category::User => "user",
            Category::Assistant => "assistant",
            Category::ToolResults => "tool_results",
            Category::Overhead => "overhead",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Category::SystemBase => "System prompt",
            Category::SystemSkills => "Skill and plugin index",
            Category::SkillsLoaded => "Loaded skills",
            Category::SystemMemory => "Memory instructions",
            Category::SystemCitations => "Web citation rules",
            Category::SystemProject => "Project instructions",
            Category::ToolsBuiltin => "Built-in tools",
            Category::ToolsMcp => "MCP tools",
            Category::ToolsPlugins => "Plugin tools",
            Category::Summary => "Conversation summary",
            Category::Manifest => "Workflow context (manifest and instruction files)",
            Category::Memories => "Recalled memories",
            Category::Attachments => "Attachments",
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

/// The tags of the block a compaction carries workflow context in (see `compact`); its items
/// may hold manifests of their own, so it closes only at its own marker.
pub(crate) const KEPT_OPEN: &str = "<context-manifest kept=\"true\">";
pub(crate) const KEPT_CLOSE: &str = "\n[end of kept context]\n</context-manifest>";

/// The preamble of a user message the agent did not build in this process (a conversation
/// restored after a restart): the summary, manifest, and recalled memories it wrote are
/// recognised by the tags it wraps them in.
pub(crate) fn restored_spans(content: &str) -> Spans {
    let mut spans = Vec::new();
    let mut start = 0;
    for (category, open, close) in [
        (
            Category::Summary,
            "<conversation-summary>",
            "</conversation-summary>",
        ),
        (Category::Manifest, KEPT_OPEN, KEPT_CLOSE),
        (
            Category::Manifest,
            "<workflow-instructions>",
            "</workflow-instructions>",
        ),
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
        (Category::Attachments, "<attachments>", "</attachments>"),
    ] {
        // A tag may repeat (compaction keeps instruction files before newer ones).
        loop {
            let rest = &content[start..];
            let Some(end) = rest.starts_with(open).then(|| rest.find(close)).flatten() else {
                break;
            };
            let end = start + end + close.len();
            spans.push((category, start..end));
            start = (end + PART_SEPARATOR.len()).min(content.len());
        }
    }
    spans.push((Category::User, start..content.len()));
    spans
}

/// The tool definitions as sent, one JSON array per category: built-in tools, then each of
/// `groups` (outside tools: MCP servers' and activated plugins'), whose specs occupy the given
/// ranges of `specs`.
pub(crate) fn tool_texts(
    specs: &[ToolSpec],
    groups: &[(Category, Range<usize>)],
) -> Vec<(Category, String)> {
    let wire = |category: Category, tools: Vec<&ToolSpec>| {
        let array: Vec<_> = tools.into_iter().map(ToolSpec::to_wire).collect();
        (!array.is_empty()).then(|| {
            let text = serde_json::to_string(&array).unwrap_or_default();
            (category, text)
        })
    };
    let in_group = |index: usize| groups.iter().any(|(_, range)| range.contains(&index));
    let builtin = specs
        .iter()
        .enumerate()
        .filter(|(index, _)| !in_group(*index))
        .map(|(_, spec)| spec)
        .collect();
    std::iter::once(wire(Category::ToolsBuiltin, builtin))
        .chain(
            groups
                .iter()
                .map(|(category, range)| wire(*category, specs[range.clone()].iter().collect())),
        )
        .flatten()
        .collect()
}

/// Whether a category is outside tools' definitions (rendered after the built-in ones).
fn outside_tools(category: Category) -> bool {
    matches!(category, Category::ToolsMcp | Category::ToolsPlugins)
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
        self.indexed_segments()
            .into_iter()
            .map(|(_, category, text)| (category, text))
            .collect()
    }

    /// The segments, each with the index of the message it comes from (`None` for the tools).
    fn indexed_segments(&self) -> Vec<(Option<usize>, Category, &'a str)> {
        let mut segments: Vec<(Option<usize>, Category, &'a str)> = Vec::new();
        // Skill instructions loaded on use arrive as `load_skill` results.
        let skill_loads: HashSet<&str> = self
            .messages
            .iter()
            .filter_map(|message| match message {
                ChatMessage::Assistant { tool_calls, .. } => Some(tool_calls),
                _ => None,
            })
            .flatten()
            .filter(|call| call.function.name == LOAD_SKILL)
            .map(|call| call.id.as_str())
            .collect();
        for (index, message) in self.messages.iter().enumerate() {
            let recorded = self.spans.get(&index);
            let mut parts: Vec<(Category, &'a str)> = Vec::new();
            match message {
                ChatMessage::System { content } => match recorded {
                    Some(spans) => slices(content, spans, &mut parts),
                    None => parts.push((Category::SystemBase, content)),
                },
                ChatMessage::User { content, .. } => match recorded {
                    Some(spans) => slices(content, spans, &mut parts),
                    None => slices(content, &restored_spans(content), &mut parts),
                },
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    if let Some(content) = content {
                        parts.push((Category::Assistant, content));
                    }
                    for call in tool_calls {
                        parts.push((Category::Assistant, &call.function.name));
                        parts.push((Category::Assistant, &call.function.arguments));
                    }
                }
                ChatMessage::Tool {
                    content,
                    tool_call_id,
                } => {
                    let category = if skill_loads.contains(tool_call_id.as_str()) {
                        Category::SkillsLoaded
                    } else {
                        Category::ToolResults
                    };
                    parts.push((category, content));
                }
            }
            segments.extend(
                parts
                    .into_iter()
                    .map(|(category, text)| (Some(index), category, text)),
            );
        }
        for (category, text) in self.tools {
            segments.push((None, *category, text));
        }
        segments.retain(|(_, _, text)| !text.is_empty());
        segments
    }

    /// A rough count of the formatting the chat template adds: role markers per message, a
    /// wrapper per tool call, the tool block, and the reply's opening.
    fn formatting_estimate(&self) -> u64 {
        let messages: u64 = self.messages.iter().map(message_formatting).sum();
        messages + self.fixed_formatting()
    }

    /// The formatting a request has whatever its messages: the tool block and the reply's
    /// opening.
    fn fixed_formatting(&self) -> u64 {
        let tools = if self.tools.is_empty() { 0 } else { 12 };
        tools + 3
    }

    /// The whole request estimated with the heuristic, formatting included.
    pub(crate) fn estimate(&self) -> u64 {
        let text: u64 = self
            .segments()
            .iter()
            .map(|(_, text)| estimate_tokens(text))
            .sum();
        text + self.image_tokens() + self.formatting_estimate()
    }

    /// The attached images' estimated tokens ([`IMAGE_TOKENS`] each).
    fn image_tokens(&self) -> u64 {
        self.messages.iter().map(message_images).sum::<u64>() * IMAGE_TOKENS
    }
}

/// How many images a message carries.
fn message_images(message: &ChatMessage) -> u64 {
    match message {
        ChatMessage::User { images, .. } => images.len() as u64,
        _ => 0,
    }
}

/// The formatting a chat template adds around one message: its role markers and a wrapper per
/// tool call.
fn message_formatting(message: &ChatMessage) -> u64 {
    let calls = match message {
        ChatMessage::Assistant { tool_calls, .. } => tool_calls.len(),
        _ => 0,
    };
    (4 + 4 * calls) as u64
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

/// What a breakdown knows about the call it describes.
pub(crate) struct Call<'a> {
    /// The client that made the call; its tokenizer, if any, counts exactly.
    pub client: &'a dyn ChatClient,
    pub model: &'a ResolvedModel,
    pub usage: Option<Usage>,
}

/// A tool set's (built-in, MCP) tokens as a template renders them, or `None` when the server
/// cannot render templates.
type RenderedTools = Option<(u64, u64)>;

/// What a model's tokenizer said about a request.
struct Tokenized {
    /// Each segment's tokens; `None` when there is no tokenizer or it failed.
    counts: Option<Vec<u64>>,
    rendered_tools: RenderedTools,
    /// The server's context window.
    window: Option<u64>,
}

/// A conversation's size as one model would read it, message by message, for fitting it to
/// that model's context window.
#[derive(Debug, Clone)]
pub(crate) struct Measured {
    /// Each message's tokens, its share of the formatting included.
    pub messages: Vec<u64>,
    /// The tool definitions and the request's fixed formatting.
    pub fixed: u64,
    pub method: TokenCountMethod,
    /// The model's context window: its server's, else the model table's; `None` when unknown.
    pub window: Option<u64>,
}

impl Measured {
    pub(crate) fn total(&self) -> u64 {
        self.fixed + self.messages.iter().sum::<u64>()
    }
}

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
        let Tokenized {
            counts: exact,
            rendered_tools,
            window,
        } = self
            .tokenize(call.client, call.model, &segments, request.tools)
            .await;
        let mut totals = [0u64; Category::ALL.len()];
        match &exact {
            Some(counts) => {
                for ((category, _), count) in segments.iter().zip(counts) {
                    totals[category.index()] += count;
                }
                // The tools as the template renders them, not as the JSON sent; outside tools
                // are split between MCP and plugin tools as their JSON is.
                if let Some((builtin, outside)) = rendered_tools {
                    totals[Category::ToolsBuiltin.index()] = builtin;
                    let mcp = totals[Category::ToolsMcp.index()];
                    let plugins = totals[Category::ToolsPlugins.index()];
                    let split = calibrate(&[mcp, plugins], outside);
                    totals[Category::ToolsMcp.index()] = split[0];
                    totals[Category::ToolsPlugins.index()] = split[1];
                }
            }
            None => {
                for (category, text) in &segments {
                    totals[category.index()] += estimate_tokens(text);
                }
            }
        }
        // Images have no text to count; they are estimated with the attachments.
        totals[Category::Attachments.index()] += request.image_tokens();
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

    /// Counts `segments` with the client's tokenizer, if it has one, within the time budget:
    /// each segment's count (when the tokenizer answered), the tool block as the template
    /// renders it, and the server's context window.
    async fn tokenize(
        &self,
        client: &dyn ChatClient,
        model: &ResolvedModel,
        segments: &[(Category, &str)],
        tools: &[(Category, String)],
    ) -> Tokenized {
        let mut tokenized = Tokenized {
            counts: None,
            rendered_tools: None,
            window: None,
        };
        // A client with a tokenizer (llama.cpp) has counts and properties to ask.
        let Some(tokenizer) = client.tokenizer() else {
            return tokenized;
        };
        let root = tokenizer.server().to_string();
        let refused = lock(&self.no_tokenizer).contains(&root);
        let work = async {
            let counted = if refused {
                None
            } else {
                Some(self.count_all(tokenizer, &model.spec, segments).await)
            };
            let rendered = match &counted {
                Some(Ok(_)) => self.rendered_tools(tokenizer, &model.spec, tools).await,
                _ => None,
            };
            (counted, rendered, self.window(tokenizer, &root).await)
        };
        match tokio::time::timeout(TOKENIZE_BUDGET, work).await {
            Ok((counted, rendered, server_window)) => {
                tokenized.window = server_window;
                tokenized.rendered_tools = rendered;
                match counted {
                    None => {}
                    Some(Ok(counts)) => tokenized.counts = Some(counts),
                    Some(Err(error)) => {
                        tracing::debug!(model = %model.spec, %error, "tokenizer unavailable; calibrating");
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
                tracing::debug!(model = %model.spec, "tokenizing took too long; calibrating");
            }
        }
        tokenized
    }

    /// Measures `request` as `model` would read it, without sending it, to fit a conversation
    /// to the model's context window. Counts are exact with the model's tokenizer (through
    /// `client`) when it has one; otherwise they are the heuristic's, multiplied by `scale`
    /// when one is known (calibrated against an earlier reported prompt size), else estimated.
    /// The chat template's formatting is always estimated.
    pub(crate) async fn measure(
        &self,
        client: &dyn ChatClient,
        model: &ResolvedModel,
        request: &Request<'_>,
        scale: Option<f64>,
    ) -> Measured {
        let segments: Vec<(Category, &str)> = request
            .indexed_segments()
            .iter()
            .map(|(_, category, text)| (*category, *text))
            .collect();
        let tokenized = self.tokenize(client, model, &segments, request.tools).await;
        let window = tokenized.window.or(model.context_window);
        assemble(request, tokenized, scale, window)
    }

    /// Estimates `request` for a model without asking anything of its server (which may not be
    /// running): the heuristic's counts, against `window` (what the model is expected to offer).
    pub(crate) fn estimate(request: &Request<'_>, window: Option<u64>) -> Measured {
        let tokenized = Tokenized {
            counts: None,
            rendered_tools: None,
            window: None,
        };
        assemble(request, tokenized, None, window)
    }
}

/// A measurement from what the tokenizer said (or the heuristic, scaled by `scale` when given).
fn assemble(
    request: &Request<'_>,
    tokenized: Tokenized,
    scale: Option<f64>,
    window: Option<u64>,
) -> Measured {
    let indexed = request.indexed_segments();
    let segments: Vec<(Category, &str)> = indexed
        .iter()
        .map(|(_, category, text)| (*category, *text))
        .collect();
    let (counts, method) = match tokenized.counts {
        Some(counts) => (counts, TokenCountMethod::Exact),
        None => {
            let factor = scale.unwrap_or(1.0);
            let counts = segments
                .iter()
                .map(|(_, text)| (estimate_tokens(text) as f64 * factor).ceil() as u64)
                .collect();
            let method = if scale.is_some() {
                TokenCountMethod::Calibrated
            } else {
                TokenCountMethod::Estimated
            };
            (counts, method)
        }
    };
    let mut messages: Vec<u64> = request
        .messages
        .iter()
        .map(|message| message_formatting(message) + message_images(message) * IMAGE_TOKENS)
        .collect();
    let mut tools = [0u64; 2];
    for ((index, category, _), count) in indexed.iter().zip(&counts) {
        match index {
            Some(index) => messages[*index] += count,
            None if outside_tools(*category) => tools[1] += count,
            None => tools[0] += count,
        }
    }
    if method == TokenCountMethod::Exact
        && let Some((builtin, mcp)) = tokenized.rendered_tools
    {
        tools = [builtin, mcp];
    }
    Measured {
        messages,
        fixed: tools[0] + tools[1] + request.fixed_formatting(),
        method,
        window,
    }
}

impl TokenCounter {
    /// Every segment's count from the tokenizer, asking the server only for text it has not
    /// counted before.
    async fn count_all(
        &self,
        tokenizer: &dyn Tokenizer,
        model: &str,
        segments: &[(Category, &str)],
    ) -> Result<Vec<u64>, ModelError> {
        let mut counts = Vec::with_capacity(segments.len());
        for (_, text) in segments {
            let key = (model.to_string(), content_hash(text), text.len());
            let cached = lock(&self.counts).get(&key).copied();
            let count = match cached {
                Some(count) => count,
                None => {
                    let count = tokenizer.count_tokens(text).await?;
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

    /// The tokens the model's chat template spends on `tools` (built-in, outside): the difference
    /// it makes to rendering a minimal conversation, so the template's tool instructions and
    /// its own JSON spacing count as tools. Measured once per tool set; `None` when the server
    /// has no `/apply-template`, leaving the JSON as sent.
    async fn rendered_tools(
        &self,
        tokenizer: &dyn Tokenizer,
        model: &str,
        tools: &[(Category, String)],
    ) -> Option<(u64, u64)> {
        if tools.is_empty() {
            return None;
        }
        let joined: String = tools.iter().map(|(_, text)| text.as_str()).collect();
        let key = (model.to_string(), content_hash(&joined));
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
            .chain(array(Category::ToolsPlugins))
            .collect();
        let probe = serde_json::json!([
            {"role": "system", "content": "x"},
            {"role": "user", "content": "x"},
        ]);
        let measured = async {
            let bare = tokenizer.template_tokens(&probe, &[]).await?;
            let with_builtin = tokenizer.template_tokens(&probe, &builtin).await?;
            let with_all = tokenizer.template_tokens(&probe, &all).await?;
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
                tracing::debug!(%model, %error, "could not render the tool block");
                return None;
            }
        };
        lock(&self.rendered_tools).insert(key, rendered);
        rendered
    }

    /// The context window the server behind `client` reports, when it has a tokenizer and
    /// answers in time.
    pub(crate) async fn server_window(&self, client: &dyn ChatClient) -> Option<u64> {
        let tokenizer = client.tokenizer()?;
        let root = tokenizer.server().to_string();
        tokio::time::timeout(TOKENIZE_BUDGET, self.window(tokenizer, &root))
            .await
            .ok()
            .flatten()
    }

    /// The server's context window, asked once per server. A failed connection is asked again
    /// next time; a server that answers without one is not.
    async fn window(&self, tokenizer: &dyn Tokenizer, root: &str) -> Option<u64> {
        if let Some(window) = lock(&self.windows).get(root) {
            return *window;
        }
        let window = match tokenizer.context_window().await {
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
            ChatMessage::user(user),
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
        let tools = tool_texts(
            &[spec("read_file"), spec("mcp__s__t"), spec("mcp__p_s__t")],
            &[(Category::ToolsMcp, 1..2), (Category::ToolsPlugins, 2..3)],
        );
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
        let plugins: serde_json::Value =
            serde_json::from_str(text_of(Category::ToolsPlugins)[0]).unwrap();
        assert_eq!(plugins, json!([spec("mcp__p_s__t").to_wire()]));
    }

    #[test]
    fn loaded_skills_count_apart_from_other_tool_results() {
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        };
        let messages = vec![
            ChatMessage::System {
                content: "base".into(),
            },
            ChatMessage::user("go"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![call("a", "load_skill"), call("b", "read_file")],
            },
            ChatMessage::Tool {
                tool_call_id: "a".into(),
                content: "# Skill: pdf".into(),
            },
            ChatMessage::Tool {
                tool_call_id: "b".into(),
                content: "file text".into(),
            },
        ];
        let spans = HashMap::new();
        let request = Request {
            messages: &messages,
            spans: &spans,
            tools: &[],
        };
        let segments = request.segments();
        assert!(segments.contains(&(Category::SkillsLoaded, "# Skill: pdf")));
        assert!(segments.contains(&(Category::ToolResults, "file text")));
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
}
