//! A hook before each model call and subagent spawn, so a used-up budget stops spend in a
//! turn that is already running. The engine knows nothing about budgets: it asks the daemon
//! whether a call to a model may go ahead and acts on the answer.

/// What the daemon says about the next call to a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// The call may go ahead.
    Go,
    /// A budget that blocks is used up: the call (or spawn) must not happen.
    Blocked(String),
    /// A budget that asks is used up: the call waits for the person's approval, then
    /// [`BudgetGate::confirm`] is told with `key`.
    Ask { message: String, key: String },
}

/// Asked before every model call an agent makes and before a subagent is spawned.
pub trait BudgetGate: std::fmt::Debug + Send + Sync {
    /// Whether a call to the model `spec` may go ahead now. Cheap and synchronous.
    fn check(&self, spec: &str) -> BudgetVerdict;
    /// The person approved a [`BudgetVerdict::Ask`]: later calls under `key` go ahead.
    fn confirm(&self, key: &str);
}
