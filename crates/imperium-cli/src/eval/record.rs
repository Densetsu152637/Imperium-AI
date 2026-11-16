//! A model client that wraps the real one for an evaluation run: it applies the variant to the
//! root agent's requests, keeps what was sent and what came back, and keeps the token and cost
//! ledger the ceilings read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use imperium_api::{ChatMessage, Pricing, ToolCall, Usage};
use imperium_engine::{BudgetGate, BudgetVerdict, ChatClient, ChatRequest};
use imperium_models::{ChatFuture, Completion, ModelError};
use serde_json::{Value, json};

use super::variant::{Placeholders, Variant};

/// How a request is known as the root agent's: its system prompt opens like this (the engine's
/// root prompt, before any variant edit).
const ROOT_PREFIX: &str = "You are Imperium, a coding agent";

/// What the stand-in model says in `--print-prompt` mode, where no model is called.
const PRINT_REPLY: &str = "(print-prompt: no model was called)";

/// What one model request carried to the root agent, as sent.
#[derive(Debug, Clone)]
pub struct RootRequest {
    pub system_prompt: String,
    /// Every message in the Chat Completions wire shape.
    pub messages: Vec<Value>,
    /// Every tool in the Chat Completions wire shape.
    pub tools: Vec<Value>,
}

/// Tokens and cost of the calls of one kind of agent.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ledger {
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost_usd: f64,
}

impl Ledger {
    fn add(&mut self, usage: &Usage, cost_usd: f64) {
        self.calls += 1;
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.cached_tokens += usage.cached_tokens;
        self.cost_usd += cost_usd;
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// The ledger as JSON; the cost is `null` when no prices were given.
    pub fn to_json(self, priced: bool) -> Value {
        json!({
            "model_calls": self.calls,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "cached_tokens": self.cached_tokens,
            "cost_usd": if priced { json!(self.cost_usd) } else { Value::Null },
        })
    }
}

/// Everything a run keeps about its model calls.
#[derive(Debug, Default)]
pub struct Record {
    /// The root agent's first request: its system prompt and tools, as the model first saw them.
    pub first_root: Option<RootRequest>,
    /// The root agent's latest request.
    pub last_root: Option<RootRequest>,
    /// The assistant message the latest root request got back (reasoning is not kept).
    pub last_reply: Option<Value>,
    pub root: Ledger,
    pub subagents: Ledger,
    /// Calls whose endpoint reported no usage, which are counted from text length instead.
    pub estimated_calls: u64,
    /// Variant edits that found nothing to change, once each.
    pub warnings: Vec<String>,
    /// Errors the endpoint returned, in order.
    pub errors: Vec<String>,
    /// Set when a ceiling stopped the run: `cost_ceiling` or `token_ceiling`.
    pub ceiling: Option<&'static str>,
}

/// The record a run's client and ceilings share.
pub type Shared = Arc<Mutex<Record>>;

/// Locks the record, going on with what it holds if a thread panicked while holding it.
pub fn lock(record: &Shared) -> MutexGuard<'_, Record> {
    record.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops the run before a model call when a ceiling is used up.
#[derive(Debug)]
pub struct Ceilings {
    pub record: Shared,
    pub cost_usd: Option<f64>,
    pub tokens: Option<u64>,
}

impl BudgetGate for Ceilings {
    fn check(&self, _spec: &str) -> BudgetVerdict {
        let mut record = lock(&self.record);
        if let Some(limit) = self.cost_usd {
            let spent = record.root.cost_usd + record.subagents.cost_usd;
            if spent >= limit {
                record.ceiling = Some("cost_ceiling");
                return BudgetVerdict::Blocked(format!(
                    "the run's cost ceiling of ${limit} is used up (${spent:.6} spent)"
                ));
            }
        }
        if let Some(limit) = self.tokens {
            let used = record.root.total_tokens() + record.subagents.total_tokens();
            if used >= limit {
                record.ceiling = Some("token_ceiling");
                return BudgetVerdict::Blocked(format!(
                    "the run's token ceiling of {limit} is used up ({used} tokens used)"
                ));
            }
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, _key: &str) {}
}

/// The client the session uses in place of the endpoint's own.
#[derive(Debug)]
pub struct Recorder {
    /// The real client; `None` answers every call with a fixed reply (`--print-prompt`).
    pub inner: Option<Arc<dyn ChatClient>>,
    pub record: Shared,
    pub variant: Arc<Variant>,
    pub placeholders: Placeholders,
    pub pricing: Option<Pricing>,
}

impl ChatClient for Recorder {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(self.call(request, on_text))
    }
}

impl Recorder {
    async fn call(
        &self,
        request: ChatRequest<'_>,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Completion, ModelError> {
        let root = matches!(
            request.messages.first(),
            Some(ChatMessage::System { content }) if content.starts_with(ROOT_PREFIX)
        );
        let mut messages = request.messages.to_vec();
        let mut tools = request.tools.to_vec();
        let mut back: HashMap<String, String> = HashMap::new();
        if root {
            let mut warnings = Vec::new();
            if let Some(ChatMessage::System { content }) = messages.first_mut() {
                *content = self
                    .variant
                    .edit_prompt(content, self.placeholders, &mut warnings);
            }
            self.variant.rename_calls(&mut messages);
            let (edited, reverse) = self.variant.edit_tools(&tools, &mut warnings);
            tools = edited;
            back = reverse;
            self.remember_request(&messages, &tools, warnings);
        }
        let sent = ChatRequest {
            model: request.model,
            messages: &messages,
            tools: &tools,
            slot: request.slot,
            max_tokens: request.max_tokens,
            effort: request.effort,
        };
        let result = match &self.inner {
            Some(inner) => inner.stream(sent, on_text).await,
            None => Ok(Completion {
                text: PRINT_REPLY.into(),
                ..Completion::default()
            }),
        };
        let mut completion = match result {
            Ok(completion) => completion,
            Err(error) => {
                lock(&self.record).errors.push(error.to_string());
                return Err(error);
            }
        };
        let reported = completion.usage;
        let usage = reported.unwrap_or_else(|| estimate(&messages, &completion));
        self.account(root, &usage, reported.is_none());
        if root {
            self.remember_reply(&completion);
            for call in &mut completion.tool_calls {
                if let Some(original) = back.get(&call.function.name) {
                    call.function.name.clone_from(original);
                }
            }
        }
        Ok(completion)
    }

    fn remember_request(
        &self,
        messages: &[ChatMessage],
        tools: &[imperium_api::ToolSpec],
        warnings: Vec<String>,
    ) {
        let system_prompt = match messages.first() {
            Some(ChatMessage::System { content }) => content.clone(),
            _ => String::new(),
        };
        let request = RootRequest {
            system_prompt,
            messages: messages.iter().map(ChatMessage::to_wire).collect(),
            tools: tools.iter().map(imperium_api::ToolSpec::to_wire).collect(),
        };
        let mut record = lock(&self.record);
        for warning in warnings {
            if !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        if record.first_root.is_none() {
            record.first_root = Some(request.clone());
        }
        record.last_root = Some(request);
    }

    fn remember_reply(&self, completion: &Completion) {
        let message = assistant_message(&completion.text, &completion.tool_calls);
        lock(&self.record).last_reply = Some(message.to_wire());
    }

    fn account(&self, root: bool, usage: &Usage, estimated: bool) {
        let cost = self
            .pricing
            .as_ref()
            .map_or(0.0, |pricing| pricing.cost_usd(usage));
        let mut record = lock(&self.record);
        if estimated {
            record.estimated_calls += 1;
        }
        if root {
            record.root.add(usage, cost);
        } else {
            record.subagents.add(usage, cost);
        }
    }
}

/// An assistant message with `text` and `tool_calls`, as the engine would keep it.
pub fn assistant_message(text: &str, tool_calls: &[ToolCall]) -> ChatMessage {
    ChatMessage::Assistant {
        content: (!text.is_empty()).then(|| text.to_string()),
        tool_calls: tool_calls.to_vec(),
    }
}

/// Tokens counted from text length (four characters each) for an endpoint that reports none.
fn estimate(messages: &[ChatMessage], completion: &Completion) -> Usage {
    let prompt_chars: usize = messages
        .iter()
        .map(|message| message.to_wire().to_string().len())
        .sum();
    let reply_chars = completion.text.len()
        + completion
            .tool_calls
            .iter()
            .map(|call| call.function.name.len() + call.function.arguments.len())
            .sum::<usize>();
    Usage {
        prompt_tokens: (prompt_chars / 4) as u64,
        completion_tokens: (reply_chars / 4) as u64,
        cached_tokens: 0,
    }
}
