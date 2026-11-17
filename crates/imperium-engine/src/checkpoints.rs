//! A hook around each root turn, so the daemon can record a checkpoint of a session's files
//! before a turn that changes them (git-based, in `imperium-core`). The engine knows nothing
//! about git: it only says when a root turn starts and ends.

/// Called around each root turn. `before_turn` and `after_turn` may block (they can read every
/// file in the session's folders), so the session runs them on the blocking pool. The turn does
/// not wait for `before_turn`: its model call starts beside it, and only a tool call waits for
/// it. `after_turn` follows once it is done.
pub trait TurnCheckpoints: std::fmt::Debug + Send + Sync {
    /// Just before a root turn begins, on the session's own task: notes where the conversation
    /// stands, which the turn is about to add to. Quick; `before_turn` then follows beside the
    /// turn.
    fn begin_turn(&self) {}
    /// Records the files before a root turn on `prompt` (the user's message as the agent will
    /// see it), beside the turn: no tool of the turn runs until this returns.
    fn before_turn(&self, prompt: &str);
    /// After the turn ended, however it ended.
    fn after_turn(&self);
}
