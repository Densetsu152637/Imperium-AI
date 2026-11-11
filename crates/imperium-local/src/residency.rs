//! The vocabulary between the residency controller (owned by `imperium-core`) and the supervisor
//! that carries its decisions out: what can be done to memory ([`Action`]), why ([`Reason`]),
//! what was done ([`Move`]), and the policy the supervisor asks when a call needs room
//! ([`ResidencyPolicy`]).
//!
//! The supervisor has no timers and starts no task of its own that loads, unloads or saves
//! anything. It moves memory only:
//!
//! - for a call that needs a model (a load, its evictions, the restore of the caller's saved
//!   context, a pool that must grow, or a model moved to the GPU under the GPU rule), deciding
//!   how through the installed [`ResidencyPolicy`];
//! - when told to, through [`crate::supervisor::Supervisor::execute`] (idle unloads, settings
//!   restarts, `models.unload`, a stopped session's slots, a resuming agent's context, the
//!   memory budget, a crashed server taken out of the loaded set).
//!
//! Every move is reported as a [`Move`] with its [`Reason`]
//! ([`crate::supervisor::Supervisor::moves`]).

use std::fmt;
use std::time::Duration;

use imperium_api::Priority;

use crate::arbiter::{self, Decision, Policy, Resident};
use crate::planner::{Budget, Placement};

/// Something done to what is in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start a model's server.
    Load { model: String },
    /// Stop a model's server, saving the slots of agents that can come back.
    Unload { model: String },
    /// Save an agent's slot to disk and free its KV cells.
    SaveSlot { model: String, holder: String },
    /// Bring an agent's saved slot back (loading its model first if needed).
    RestoreSlot { model: String, holder: String },
    /// Restart a model with a KV pool of `tokens`, its agents' slots saved and restored.
    ResizePool { model: String, tokens: u32 },
    /// Plan against this much memory from now on.
    SetBudget { vram_bytes: u64, ram_bytes: u64 },
}

impl Action {
    /// The action's name on the wire (`load`, `unload`, `save_slot`, ...).
    pub fn name(&self) -> &'static str {
        match self {
            Action::Load { .. } => "load",
            Action::Unload { .. } => "unload",
            Action::SaveSlot { .. } => "save_slot",
            Action::RestoreSlot { .. } => "restore_slot",
            Action::ResizePool { .. } => "resize_pool",
            Action::SetBudget { .. } => "set_budget",
        }
    }

    /// The model it moves; `None` for the budget.
    pub fn model(&self) -> Option<&str> {
        match self {
            Action::Load { model }
            | Action::Unload { model }
            | Action::SaveSlot { model, .. }
            | Action::RestoreSlot { model, .. }
            | Action::ResizePool { model, .. } => Some(model),
            Action::SetBudget { .. } => None,
        }
    }

    /// The agent whose slot it moves.
    pub fn holder(&self) -> Option<&str> {
        match self {
            Action::SaveSlot { holder, .. } | Action::RestoreSlot { holder, .. } => Some(holder),
            _ => None,
        }
    }
}

/// Why something moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// A model call needs it (a load, or the caller's saved context coming back).
    Call,
    /// An agent goes on after waiting (subagents done, a steering message, Resume): its context
    /// comes back ahead of its next call.
    Resume,
    /// Another model needed the memory: it did not fit beside this one.
    Room,
    /// Another model gets more of its layers on the GPU once this one is gone.
    MoreGpu,
    /// Another model gets more than `swap_gain` times the GPU memory once this one is gone.
    SwapGain,
    /// The GPU rule: nothing was generating, so everything not generating gave way rather than
    /// let a call run on the CPU.
    GpuRule,
    /// The critic (a CPU-lane model) was placed on the GPU while the agent it reviews waits.
    Critic,
    /// Nothing else fitted and this model, held by an agent mid-turn, had been loaded at least
    /// `min_residency_s`.
    Aged,
    /// Unused for `idle_unload_s`.
    Idle,
    /// Its settings changed: it restarts with the new ones.
    Config,
    /// A call needs a larger KV pool.
    Grow,
    /// It ran (partly) on the CPU while the GPU was idle: it restarts to be placed on the GPU.
    Promote,
    /// `models.unload`.
    Request,
    /// A waiting agent gave its KV cells to another agent on the same model.
    Yield,
    /// The user stopped the session.
    Stopped,
    /// The free memory measured on the machine changed.
    Budget,
    /// Its server exited on its own: it is taken out of the loaded set, and the next call for
    /// it loads it again (after [`crash_backoff`] when it keeps crashing).
    Crashed,
}

/// How long a model that crashed `streak` times in a row stays down before a call may load it
/// again: not at all after one crash, then 5 s, doubling up to 5 minutes, so a server that
/// dies as soon as it loads does not spin.
pub fn crash_backoff(streak: u32) -> Duration {
    const FIRST: Duration = Duration::from_secs(5);
    const MOST: Duration = Duration::from_secs(5 * 60);
    match streak {
        0 | 1 => Duration::ZERO,
        n => FIRST
            .checked_mul(1 << (n - 2).min(16))
            .map_or(MOST, |wait| wait.min(MOST)),
    }
}

impl Reason {
    /// The reason's name on the wire (`call`, `room`, `gpu_rule`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Reason::Call => "call",
            Reason::Resume => "resume",
            Reason::Room => "room",
            Reason::MoreGpu => "more_gpu",
            Reason::SwapGain => "swap_gain",
            Reason::GpuRule => "gpu_rule",
            Reason::Critic => "critic",
            Reason::Aged => "aged",
            Reason::Idle => "idle",
            Reason::Config => "config",
            Reason::Grow => "grow",
            Reason::Promote => "promote",
            Reason::Request => "request",
            Reason::Yield => "yield",
            Reason::Stopped => "stopped",
            Reason::Budget => "budget",
            Reason::Crashed => "crashed",
        }
    }

    /// The coarser reason a swap-out reports (`model.swapped_out`): every eviction made for
    /// another model's load is `room`.
    pub fn swap_label(self) -> &'static str {
        match self {
            Reason::Room
            | Reason::MoreGpu
            | Reason::SwapGain
            | Reason::GpuRule
            | Reason::Critic
            | Reason::Aged => "room",
            other => other.name(),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One thing the supervisor did to memory, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub action: Action,
    pub reason: Reason,
    /// Whether it was done; when not, `detail` says why.
    pub ok: bool,
    /// What else is worth knowing: the model a swap-out made room for, or the error.
    pub detail: Option<String>,
    /// How long it took.
    pub seconds: f64,
    /// Bytes it moved: slot state written or read, memory a load took.
    pub bytes: u64,
}

impl Move {
    pub(crate) fn done(action: Action, reason: Reason) -> Self {
        Move {
            action,
            reason,
            ok: true,
            detail: None,
            seconds: 0.0,
            bytes: 0,
        }
    }

    pub(crate) fn failed(action: Action, reason: Reason, error: impl Into<String>) -> Self {
        Move {
            ok: false,
            detail: Some(error.into()),
            ..Move::done(action, reason)
        }
    }

    pub(crate) fn took(mut self, seconds: f64, bytes: u64) -> Self {
        self.seconds = seconds;
        self.bytes = bytes;
        self
    }

    pub(crate) fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// What a call that needs a model not loaded brings to the policy.
pub struct LoadAsk<'a> {
    /// The model the call needs.
    pub model: &'a str,
    /// The agent calling (`<session>/<agent>`).
    pub holder: &'a str,
    pub priority: Priority,
    /// Where the model goes within a budget, as the runtime places it.
    pub place: &'a dyn Fn(Budget) -> Option<Placement>,
    /// The memory budget now.
    pub budget: Budget,
    /// The loaded models, from the caller's point of view.
    pub residents: &'a [Resident],
    /// How long ago the call was first made.
    pub waited: Duration,
    pub limits: Policy,
}

impl fmt::Debug for LoadAsk<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadAsk")
            .field("model", &self.model)
            .field("holder", &self.holder)
            .field("priority", &self.priority)
            .field("budget", &self.budget)
            .field("residents", &self.residents)
            .field("waited", &self.waited)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// The decisions the supervisor asks for while it serves a call. `imperium-core` installs its
/// residency controller here ([`crate::supervisor::Supervisor::set_policy`]); [`Standard`] is
/// the arbiter's rules with nothing on top, for a supervisor running without the daemon.
pub trait ResidencyPolicy: Send + Sync + fmt::Debug {
    /// How to make room for a call's model: what to swap out (the decision's reason says
    /// why), or whether to wait.
    fn decide(&self, ask: &LoadAsk<'_>) -> Decision {
        arbiter::decide(ask.place, ask.budget, ask.residents, ask.waited, ask.limits)
    }

    /// Which of the agents `waiting` on `model` (for subagents, an approval, an answer) give
    /// their KV cells up now that `holder` took a slot on it: their slots are saved and freed.
    fn yield_slots(&self, _model: &str, _holder: &str, waiting: Vec<String>) -> Vec<String> {
        waiting
    }

    /// How long `model`, whose server crashed `streak` times in a row, stays down before a
    /// call may load it again.
    fn crash_backoff(&self, _model: &str, streak: u32) -> Duration {
        crash_backoff(streak)
    }
}

/// The arbiter's rules as they are.
#[derive(Debug, Default, Clone, Copy)]
pub struct Standard;

impl ResidencyPolicy for Standard {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_loop_backs_off_up_to_five_minutes() {
        let waits: Vec<u64> = (1..=9).map(|n| crash_backoff(n).as_secs()).collect();
        assert_eq!(waits, [0, 5, 10, 20, 40, 80, 160, 300, 300]);
        assert_eq!(crash_backoff(u32::MAX), Duration::from_secs(300));
    }
}
