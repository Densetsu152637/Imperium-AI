//! A model client that wraps the real one for an evaluation session: it applies the variant to
//! the root agent's requests, keeps what was sent and what came back, and keeps the token and
//! cost ledger the ceilings read.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use imperium_api::{ChatMessage, Pricing, ToolCall, ToolSpec, Usage};
use imperium_engine::{BudgetGate, BudgetVerdict, ChatClient, ChatRequest, ToolWatch};
use imperium_models::{ChatFuture, Completion, ModelError};
use imperium_protocol::methods::{EvalCalled, EvalLedger, EvalRequest, EvalSearched, EvalVariant};

use super::variant::{self, Placeholders};

/// How a request is known as the root agent's: its system prompt opens like this (the engine's
/// root prompt, before any variant edit).
const ROOT_PREFIX: &str = "You are Imperium, a coding agent";

/// What the stand-in model says when no model is to be called (`session.prompt_preview`).
const PREVIEW_REPLY: &str = "(prompt preview: no model was called)";

/// Everything a session keeps about its model calls.
#[derive(Debug, Default)]
pub(crate) struct Record {
    /// The root agent's first request: its system prompt and tools, as the model first saw them.
    pub first_root: Option<EvalRequest>,
    /// The root agent's latest request.
    pub last_root: Option<EvalRequest>,
    /// The assistant message the latest root request got back (reasoning is not kept).
    pub last_reply: Option<serde_json::Value>,
    pub root: EvalLedger,
    pub subagents: EvalLedger,
    /// Calls whose endpoint reported no usage, which are counted from text length instead.
    pub estimated_calls: u64,
    /// Variant edits that found nothing to change, once each.
    pub warnings: Vec<String>,
    /// Errors the endpoint returned, in order.
    pub errors: Vec<String>,
    /// Set when a ceiling stopped the run: `cost_ceiling` or `token_ceiling`.
    pub ceiling: Option<&'static str>,
    /// What the root agent was offered and reached on demand.
    pub tools: ToolRecord,
}

/// The root agent's tools over a run, from its requests and the engine's [`ToolWatch`]. Names
/// are the ones the model saw.
#[derive(Debug, Default)]
pub(crate) struct ToolRecord {
    /// The tools reached through `tool_search` and `call_tool`, as the latest list had them.
    pub deferred: Vec<ToolSpec>,
    pub searched: Vec<EvalSearched>,
    pub called: Vec<EvalCalled>,
    /// The compact JSON size of each root request's `tools`.
    pub bytes_by_call: Vec<u64>,
    /// How often a root request's `tools` differed from the one before.
    pub changes: u64,
    last_json: Option<String>,
}

impl ToolRecord {
    /// Notes the `tools` array of a root request.
    fn note_request(&mut self, tools: &[serde_json::Value]) {
        let json = serde_json::to_string(tools).unwrap_or_default();
        self.bytes_by_call.push(json.len() as u64);
        if self.last_json.as_ref().is_some_and(|last| *last != json) {
            self.changes += 1;
        }
        self.last_json = Some(json);
    }
}

/// What the engine tells an evaluation about the root's tools, kept in the record.
#[derive(Debug)]
pub(crate) struct Watch {
    pub record: Shared,
}

impl ToolWatch for Watch {
    fn catalog(&self, _offered: &[ToolSpec], deferred: &[ToolSpec]) {
        lock(&self.record).tools.deferred = deferred.to_vec();
    }

    fn searched(&self, names: &[String]) {
        // The root's reply that asked has been counted, and no later call has started.
        let mut record = lock(&self.record);
        let call = record.root.calls;
        record.tools.searched.push(EvalSearched {
            names: names.to_vec(),
            call,
        });
    }

    fn called(&self, name: &str) {
        let mut record = lock(&self.record);
        let call = record.root.calls;
        record.tools.called.push(EvalCalled {
            name: name.to_string(),
            call,
        });
    }

    fn warn(&self, message: &str) {
        let mut record = lock(&self.record);
        if !record.warnings.iter().any(|known| known == message) {
            record.warnings.push(message.to_string());
        }
    }
}

/// The record a session's client and ceilings share.
pub(crate) type Shared = Arc<Mutex<Record>>;

/// Locks the record, going on with what it holds if a thread panicked while holding it.
pub(crate) fn lock(record: &Shared) -> MutexGuard<'_, Record> {
    record.lock().unwrap_or_else(PoisonError::into_inner)
}

fn add(ledger: &mut EvalLedger, usage: &Usage, cost_usd: f64) {
    ledger.calls += 1;
    ledger.prompt_tokens += usage.prompt_tokens;
    ledger.completion_tokens += usage.completion_tokens;
    ledger.cached_tokens += usage.cached_tokens;
    ledger.cost_usd += cost_usd;
}

fn total_tokens(ledger: &EvalLedger) -> u64 {
    ledger.prompt_tokens + ledger.completion_tokens
}

/// Stops the run before a model call when a ceiling is used up.
#[derive(Debug)]
pub(crate) struct Ceilings {
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
            let used = total_tokens(&record.root) + total_tokens(&record.subagents);
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
pub(crate) struct Recorder {
    /// The real client; `None` answers every call with a fixed reply (`session.prompt_preview`).
    pub inner: Option<Arc<dyn ChatClient>>,
    pub record: Shared,
    pub variant: Arc<EvalVariant>,
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
        // The variant's tool edits were applied by the engine, which reverses them on the
        // reply: what arrives here is what the model is sent, and what it answers.
        let tools = request.tools.to_vec();
        if root {
            let mut warnings = Vec::new();
            if let Some(ChatMessage::System { content }) = messages.first_mut() {
                *content =
                    variant::edit_prompt(&self.variant, content, self.placeholders, &mut warnings);
            }
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
                text: PREVIEW_REPLY.into(),
                ..Completion::default()
            }),
        };
        let completion = match result {
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
        }
        Ok(completion)
    }

    fn remember_request(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        warnings: Vec<String>,
    ) {
        let system_prompt = match messages.first() {
            Some(ChatMessage::System { content }) => content.clone(),
            _ => String::new(),
        };
        let request = EvalRequest {
            system_prompt,
            messages: messages.iter().map(ChatMessage::to_wire).collect(),
            tools: tools.iter().map(ToolSpec::to_wire).collect(),
        };
        let mut record = lock(&self.record);
        record.tools.note_request(&request.tools);
        for warning in warnings {
            if !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        if record.first_root.is_none() {
            record.first_root = Some(request.clone());
        }
        record.last_root = Some(request);
        // The reply to this request is not known yet.
        record.last_reply = None;
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
            add(&mut record.root, usage, cost);
        } else {
            add(&mut record.subagents, usage, cost);
        }
    }
}

/// An assistant message with `text` and `tool_calls`, as the engine would keep it.
pub(crate) fn assistant_message(text: &str, tool_calls: &[ToolCall]) -> ChatMessage {
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
