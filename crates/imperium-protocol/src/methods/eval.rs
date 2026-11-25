//! Evaluation runs: sessions the `imperium run` command makes through the daemon with a harness
//! variant, recorded subagents, its own root model and unattended permission handling
//! (docs/03-agent-engine.md#running-evals-through-the-harness-implemented).
//!
//! These methods are not part of the desktop app's protocol, so they have no generated
//! TypeScript types. Only a client that is not a paired phone and says it is `imperium-cli` in
//! the handshake may call them.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Method, SessionCreateResult};

/// What a client says about itself in the handshake when it is the `imperium` command.
pub const EVAL_CLIENT: &str = "imperium-cli";

/// `session.eval_create`: starts an evaluation session. It works in an environment or folder
/// like `session.create` and has the user's permission rules, grants, isolation tier and
/// protected folders, but none of the user's models, memory, skills, hooks, MCP servers or
/// project instructions: the root agent has the engine's own tools and the web tools (search
/// answers with an error), and its model is `options.model`. The session is not remembered, so
/// it does not continue after a restart. Its events follow on the connection like a session's
/// from `session.create`; permission requests and questions are answered by the daemon.
#[derive(Debug)]
pub struct SessionEvalCreate;

impl Method for SessionEvalCreate {
    const NAME: &'static str = "session.eval_create";
    type Params = SessionEvalCreateParams;
    type Result = SessionCreateResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvalCreateParams {
    #[serde(default)]
    pub environment_id: Option<String>,
    /// A folder to work in, as for `session.create`.
    #[serde(default)]
    pub project_dir: Option<String>,
    pub options: EvalOptions,
}

/// `session.eval_result`: what an evaluation session's model calls, subagent answers and
/// ceilings recorded so far.
#[derive(Debug)]
pub struct SessionEvalResult;

impl Method for SessionEvalResult {
    const NAME: &'static str = "session.eval_result";
    type Params = SessionEvalResultParams;
    type Result = EvalResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvalResultParams {
    pub session_id: String,
}

/// `session.prompt_preview`: the system prompt and tool schemas a run with these options would
/// send first, from the same code that starts the session, with no model called.
#[derive(Debug)]
pub struct SessionPromptPreview;

impl Method for SessionPromptPreview {
    const NAME: &'static str = "session.prompt_preview";
    type Params = SessionEvalCreateParams;
    type Result = PromptPreview;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptPreview {
    /// The variant's `config_hash`.
    pub variant_config_hash: String,
    /// `sha256:` over the system prompt (the working folder as `{workspace}`) and the tools.
    pub prompt_hash: String,
    pub system_prompt: String,
    /// The tools in the Chat Completions `tools` shape.
    pub tools: Vec<serde_json::Value>,
    /// Variant edits that found nothing to change.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// With reactive tool loading on: the tools outside the list, with their full schemas as
    /// `tool_search` returns them (`name`, `description`, `parameters`), after the variant's
    /// edits. Empty with loading off.
    #[serde(default)]
    pub deferred_tools: Vec<serde_json::Value>,
    /// How tools are offered (the mode, the core and the deferred names).
    #[serde(default)]
    pub tool_loading: Option<EvalToolLoading>,
}

/// Everything an evaluation session is set up with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalOptions {
    pub model: EvalModel,
    /// The harness variant, resolved by the client; the daemon validates it.
    #[serde(default)]
    pub variant: EvalVariant,
    /// Answer subagents from these recordings instead of running them.
    #[serde(default)]
    pub replay: Option<Vec<EvalReply>>,
    /// Run this named agent file (`agents/<name>.md`) as the root agent. A file that lists
    /// `tools` with nothing in the list runs with no tools at all.
    #[serde(default)]
    pub agent: Option<String>,
    /// How tool calls are checked (`plan`, `ask`, `default`, `accept_edits`, `full`); `default`
    /// when unset. `auto` is refused: it asks a classifier model.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Stop before the next model call once this much has been spent (needs prices).
    #[serde(default)]
    pub cost_ceiling_usd: Option<f64>,
    /// Stop before the next model call once this many tokens have been used.
    #[serde(default)]
    pub token_ceiling: Option<u64>,
    /// The daemon stops the session after this many seconds and deletes it shortly after, so a
    /// client that vanishes leaves nothing running.
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// The root model of an evaluation session: any OpenAI-compatible endpoint, set per session
/// without touching the daemon's configuration.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalModel {
    pub base_url: String,
    pub model_id: String,
    /// Sent to the endpoint as a bearer token; never recorded or returned.
    #[serde(default)]
    pub api_key: Option<String>,
    /// An object merged into every request as `chat_template_kwargs`.
    #[serde(default)]
    pub chat_template_kwargs: Option<serde_json::Value>,
    /// The model runs on this machine: harness aids set to `auto` count as on.
    #[serde(default)]
    pub local: bool,
    /// The reasoning effort (`low`, `medium`, `high`, `max`).
    #[serde(default)]
    pub effort: Option<String>,
    /// The context window in tokens.
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// US dollars per million prompt and completion tokens, for the cost ledger.
    #[serde(default)]
    pub price_in: Option<f64>,
    #[serde(default)]
    pub price_out: Option<f64>,
}

impl fmt::Debug for EvalModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvalModel")
            .field("base_url", &self.base_url)
            .field("model_id", &self.model_id)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// A harness variant: edits to what the root agent is given, and loop flags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalVariant {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The file it came from, when it came from one.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub prompt: PromptEdits,
    #[serde(default)]
    pub tools: ToolEdits,
    /// Loop flags as text: `max_steps`, `effort`, `auto_compact`, `auto_compact_at`,
    /// `knowledge`, `attended`, `harness`, `tool_loading`.
    #[serde(default)]
    pub flags: BTreeMap<String, String>,
    /// Text appended to the system prompt after the variant's own edits.
    #[serde(default)]
    pub overlay: Option<String>,
}

/// Edits to the root agent's system prompt, applied in this order: `replace`, `remove`,
/// `prepend`, `append`. The text may use `{context_tokens}` and `{context_k}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptEdits {
    /// Text put before the base prompt, as its own paragraph.
    #[serde(default)]
    pub prepend: Option<String>,
    /// Text put after the base prompt, as its own paragraph.
    #[serde(default)]
    pub append: Option<String>,
    /// Each entry drops every paragraph (text between blank lines) that contains it, except the
    /// base paragraph.
    #[serde(default)]
    pub remove: Vec<String>,
    /// Exact text replaced by other text, first match only.
    #[serde(default)]
    pub replace: Vec<Replace>,
}

/// One exact replacement in the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replace {
    pub find: String,
    pub with: String,
}

/// Edits to the tools the root agent is offered, by the engine's tool names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEdits {
    /// Tools the model is not offered.
    #[serde(default)]
    pub drop: Vec<String>,
    /// Tools offered under another name (`old = "new"`); the engine still runs them.
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
    /// New descriptions (`name = "text"`).
    #[serde(default)]
    pub description: BTreeMap<String, String>,
    /// With reactive tool loading on: the tools offered directly (besides `tool_search` and
    /// `call_tool`), whatever the default core is; every other tool of the universe is
    /// deferred. Empty leaves the default core. Names are the engine's.
    #[serde(default)]
    pub core: Vec<String>,
}

/// The kind of subagent a recording is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvalRole {
    Task,
    Chunk,
    Reduce,
}

/// One recorded subagent report and the keys that select it. At least one key is required;
/// when several are given, all of them must match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalReply {
    /// The subagent's task id: `t1`, `t2`, ... in the order the root spawns them.
    #[serde(default)]
    pub task_id: Option<String>,
    /// The brief, exactly (blank space is not compared).
    #[serde(default)]
    pub brief: Option<String>,
    /// Text the brief must contain (compared without regard to case).
    #[serde(default)]
    pub brief_contains: Option<String>,
    /// The kind of subagent: `task`, `chunk` or `reduce`.
    #[serde(default)]
    pub role: Option<EvalRole>,
    /// For a `chunk`: its number, from 1.
    #[serde(default)]
    pub chunk: Option<usize>,
    /// What the subagent reports back.
    pub report: String,
    /// Whether the subagent counts as having finished its task. Defaults to true.
    #[serde(default = "super::yes")]
    pub ok: bool,
    /// The reply answers one subagent only: once matched it is not offered again.
    #[serde(default)]
    pub once: bool,
}

/// What an evaluation session recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub variant_config_hash: String,
    /// `sha256:` over the first request's system prompt (working folder as `{workspace}`) and
    /// tools; `None` before the root agent made a request.
    pub prompt_hash: Option<String>,
    /// The first request's system prompt with the working folder as `{workspace}`.
    pub system_prompt_template: Option<String>,
    /// The root agent's first and latest requests, as sent.
    pub first_request: Option<EvalRequest>,
    pub last_request: Option<EvalRequest>,
    /// The assistant message the latest root request got back.
    pub last_reply: Option<serde_json::Value>,
    pub root: EvalLedger,
    pub subagents: EvalLedger,
    /// Calls whose endpoint reported no usage, counted from text length.
    pub estimated_calls: u64,
    pub warnings: Vec<String>,
    /// Errors the endpoint returned, in order.
    pub errors: Vec<String>,
    /// `cost_ceiling` or `token_ceiling` when a ceiling stopped the run.
    pub ceiling: Option<String>,
    /// The subagents the recordings answered, and the recordings' size and unused entries.
    pub answered: Vec<EvalAnswered>,
    pub replay: Option<EvalReplaySummary>,
    pub permissions: Vec<EvalPermission>,
    pub questions: Vec<EvalQuestion>,
    /// The session's working folder, as the prompt shows it.
    pub workspace_root: String,
    /// How the root agent's turn ended, as the daemon saw it; `None` while it runs.
    pub ended: Option<EvalEnd>,
    /// The root agent's last plan.
    pub plan: Option<serde_json::Value>,
    /// How the root agent was offered its tools and what it reached on demand; `None` before
    /// its first request.
    #[serde(default)]
    pub tool_loading: Option<EvalToolLoading>,
}

/// Reactive tool loading in one run (docs/plans/reactive-tools.md, R7). Every name is the one the
/// model saw, after the variant's renames; `call` is the 1-based index of the root call whose
/// reply made the request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalToolLoading {
    /// `on` when the root's tool list was the fixed one with `tool_search` and `call_tool`,
    /// otherwise `off`.
    pub mode: String,
    /// The tools offered directly in the first request (without the two loader tools).
    pub core: Vec<String>,
    /// The tools reached through `tool_search` and `call_tool`, in catalog order.
    pub deferred: Vec<String>,
    /// Tools whose parameters a `tool_search` showed for the first time.
    pub searched: Vec<EvalSearched>,
    /// Calls to deferred tools, through `call_tool` or by name.
    pub called: Vec<EvalCalled>,
    /// The compact JSON size of the `tools` array of each root request, in order.
    pub tool_bytes_by_call: Vec<u64>,
    /// How many times the `tools` array differed from the request before it.
    pub tool_list_changes: u64,
    /// `sha256:` over the deferred tools' full schemas in catalog order; `None` with loading
    /// off.
    pub catalog_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalSearched {
    pub names: Vec<String>,
    pub call: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalCalled {
    pub name: String,
    pub call: u64,
}

/// How an evaluation session's root turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalEnd {
    /// `completed`, `failed`, `paused` (a usage limit) or `timeout`.
    pub reason: String,
    /// The failure or the pause's reason.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRequest {
    pub system_prompt: String,
    /// Every message in the Chat Completions wire shape.
    pub messages: Vec<serde_json::Value>,
    /// Every tool in the Chat Completions wire shape.
    pub tools: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalLedger {
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalAnswered {
    pub task_id: String,
    pub brief: String,
    pub role: EvalRole,
    /// The index of the reply that answered, or `None` for an unknown key.
    pub reply: Option<usize>,
    pub ok: bool,
    pub report: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalReplaySummary {
    pub replies: usize,
    pub unused: Vec<usize>,
}

/// A permission request the daemon denied because nobody is there to answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalPermission {
    pub agent: String,
    pub tool: String,
    pub summary: String,
    pub decision: String,
}

/// A batch of `ask_user` questions the daemon answered with the recommended answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalQuestion {
    pub agent: String,
    pub questions: Vec<String>,
    pub answered_with: String,
}
