//! Answering subagents from recordings, for evaluation harnesses.

/// What a replayed subagent reports to its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayAnswer {
    /// Whether the subagent counts as having finished its task (a failed one reports `failed`).
    pub ok: bool,
    /// The subagent's final reply, as its parent reads it.
    pub report: String,
}

/// Answers every subagent a session starts, in place of running it: no model call and no tools.
/// The answer must depend only on `task_id` and `brief`, so a replayed run is deterministic.
pub trait SubagentReplay: std::fmt::Debug + Send + Sync {
    /// The report of subagent `task_id` (`t1`, `t2`, ... in spawn order) given `brief`, the
    /// text its parent wrote (for a chunk or reduce agent of `delegate_chunks`, the call's
    /// brief). An unknown key must come back as a failed answer saying so, never as a wait.
    fn answer(&self, task_id: &str, brief: &str) -> ReplayAnswer;
}
