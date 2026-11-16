//! Answering subagents from recordings, for evaluation harnesses.

/// What a replayed subagent reports to its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayAnswer {
    /// Whether the subagent counts as having finished its task (a failed one reports `failed`).
    pub ok: bool,
    /// The subagent's final reply, as its parent reads it.
    pub report: String,
}

/// What kind of subagent is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayRole {
    /// A subagent of `spawn_agent` (or a follow-up of `resume_agent`).
    Task,
    /// Chunk `number` of `total` of a `delegate_chunks` call.
    Chunk { number: usize, total: usize },
    /// The merge agent of a `delegate_chunks` call over `chunks` chunks.
    Reduce { chunks: usize },
}

/// A subagent about to run.
#[derive(Debug, Clone, Copy)]
pub struct ReplayRequest<'a> {
    /// `t1`, `t2`, ... in spawn order.
    pub task_id: &'a str,
    /// The brief as the parent wrote it: for a chunk agent the call's `brief`, for the merge
    /// agent its `reduce` text, with no chunk text or markers added.
    pub brief: &'a str,
    pub role: ReplayRole,
}

/// Answers every subagent a session starts, in place of running it: no model call and no tools.
/// The answer must depend only on the request, so a replayed run is deterministic.
pub trait SubagentReplay: std::fmt::Debug + Send + Sync {
    /// The report of the subagent in `request`. An unknown key must come back as a failed
    /// answer saying so, never as a wait.
    fn answer(&self, request: &ReplayRequest<'_>) -> ReplayAnswer;
}
