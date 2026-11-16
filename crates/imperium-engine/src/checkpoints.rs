//! A hook around each root turn, so the daemon can record a checkpoint of a session's files
//! before a turn that changes them (git-based, in `imperium-core`). The engine knows nothing
//! about git: it only says when a root turn starts and ends.

/// Called around each root turn. Both calls may block (they can read every file in the
/// session's folders), so the session runs them on the blocking pool and waits for them.
pub trait TurnCheckpoints: std::fmt::Debug + Send + Sync {
    /// Before a root turn on `prompt` (the user's message as the agent will see it), after any
    /// compaction and before the message is added to the conversation.
    fn before_turn(&self, prompt: &str);
    /// After the turn ended, however it ended.
    fn after_turn(&self);
}
