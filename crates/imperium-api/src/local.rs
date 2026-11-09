//! The interface between routing and the supervised local runtime (implemented by
//! `imperium-local`), so the agent loop can place work on local models without depending on how
//! they are loaded.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Whether a local model can take work right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Availability {
    /// Loaded with a free slot: reusing it costs nothing.
    Loaded,
    /// Not loaded (or every slot is busy) but fits beside the loaded models: it can be loaded.
    Fits,
    /// Fits once loaded models nobody has a slot on are swapped out (their slots saved and
    /// restored when they come back). `seconds` is the expected wait for the swap, from the
    /// model's last measured load, so a caller can weigh it against a hosted model.
    Swap { seconds: u32 },
    /// Cannot run without evicting a model that is in use, swapping is paused because the
    /// swap limit was reached, or the model is unknown.
    Unavailable,
}

/// A slot on a loaded local model, held for one model call. Dropping it frees the slot, so an
/// agent takes a lease immediately before each request and drops it when the request ends, and
/// holds none while it runs tools or waits.
pub struct LocalLease {
    /// The OpenAI-compatible base URL of the model's server.
    pub base_url: String,
    /// The server slot this lease owns, for runtimes that address slots by index (llama.cpp's
    /// `id_slot`); requests sent with the lease are pinned to it so the slot's cached prompt is
    /// reused. `None` for runtimes that choose their own.
    pub slot: Option<u32>,
    /// How long the call waited for a free slot or for room to load the model, not counting
    /// time spent loading it, swapping other models out, or restoring a saved slot.
    pub waited: Duration,
    /// What the agent must send as `max_tokens` (the reservation less its prompt estimate), or
    /// `None` when the reply is uncapped or the runtime has no token pool (vLLM).
    pub max_tokens: Option<u32>,
    _release: Box<dyn Send + Sync>,
}

impl LocalLease {
    /// `release` is dropped with the lease; its `Drop` frees the slot.
    pub fn new(
        base_url: String,
        slot: Option<u32>,
        max_tokens: Option<u32>,
        release: impl Send + Sync + 'static,
    ) -> Self {
        LocalLease {
            base_url,
            slot,
            waited: Duration::ZERO,
            max_tokens,
            _release: Box::new(release),
        }
    }

    /// The lease, recording that the call waited `waited` for its slot.
    #[must_use]
    pub fn with_wait(mut self, waited: Duration) -> Self {
        self.waited = waited;
        self
    }
}

impl fmt::Debug for LocalLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalLease")
            .field("base_url", &self.base_url)
            .field("slot", &self.slot)
            .field("waited", &self.waited)
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

/// What an agent told the local runtime about itself ([`LocalModels::attach`],
/// [`LocalModels::waiting`]); dropping it withdraws that.
pub struct LocalPresence(Option<Box<dyn Send + Sync>>);

impl LocalPresence {
    /// `withdraw` is dropped with the presence; its `Drop` withdraws it.
    pub fn new(withdraw: impl Send + Sync + 'static) -> Self {
        LocalPresence(Some(Box::new(withdraw)))
    }

    /// A presence the runtime does not track.
    pub fn untracked() -> Self {
        LocalPresence(None)
    }
}

impl fmt::Debug for LocalPresence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LocalPresence")
            .field(&self.0.is_some())
            .finish()
    }
}

/// Whose work a model call is. Interactive calls are admitted before queued background calls,
/// and background work never evicts a model an interactive session used recently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Priority {
    /// A session the user is driving.
    #[default]
    Interactive,
    /// A session a schedule or a project agent started.
    Background,
}

/// What one model call asks of a local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalRequest {
    pub priority: Priority,
    /// The agent's estimate of the call's prompt, in tokens; the runtime reserves it plus the
    /// reply the model may generate. `None` reserves the whole context.
    pub prompt_tokens: Option<u32>,
    /// Reserve the whole context and send no `max_tokens`: the retry of a reply that a
    /// reservation cut short.
    pub uncapped: bool,
}

impl Default for LocalRequest {
    fn default() -> Self {
        LocalRequest {
            priority: Priority::Interactive,
            prompt_tokens: None,
            uncapped: false,
        }
    }
}

/// Why a call for a local model is waiting, while it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWait {
    /// The model is being loaded (or restarted) for it.
    Loading,
    /// The model restarts with a larger KV pool once the calls running on it are done.
    GrowingPool,
    /// The model's KV pool is full of other calls' reservations.
    KvRoom,
    /// Every slot of the model is busy.
    Slot,
    /// Another model is in the way: its calls come first, or it must unload to make room.
    OtherModel,
    /// Waiting for room on the GPU: models in the way are finishing their calls before they
    /// are swapped out, so this call does not run on the CPU.
    MakingRoom,
    /// The contexts of agents on a model being swapped out for this call are being saved.
    SavingContext,
    /// This agent's own context, saved while it waited, is being restored.
    RestoringContext,
}

/// The agent id of a holder, `<session id>/<agent id>`.
pub fn holder(session: &str, agent: &str) -> String {
    format!("{session}/{agent}")
}

/// The session a holder (see [`holder`]) belongs to: the text before the first `/`.
pub fn holder_session(holder: &str) -> &str {
    holder.split('/').next().unwrap_or(holder)
}

pub type AcquireFuture<'a> = Pin<Box<dyn Future<Output = Result<LocalLease, String>> + Send + 'a>>;

/// Local models the router may place work on.
pub trait LocalModels: Send + Sync + fmt::Debug {
    /// Where the model with this id stands, without loading anything.
    fn availability(&self, model: &str) -> Availability;
    /// Whether the runtime has a model with this id (found in the model folders), without
    /// loading anything. A runtime that cannot tell says yes; loading it then reports the error.
    fn has_model(&self, _model: &str) -> bool {
        true
    }
    /// The context window a session would get on the model once it is loaded, from its launch
    /// settings and file metadata, without loading it; `None` when unknown. Only an estimate:
    /// the loaded server's own report replaces it.
    fn planned_context(&self, _model: &str) -> Option<u64> {
        None
    }
    /// Why `holder`'s call for a model waits, while it waits in [`LocalModels::acquire`];
    /// `None` when it does not wait or the runtime does not say.
    fn wait_reason(&self, _holder: &str) -> Option<LocalWait> {
        None
    }
    /// Whether `holder`'s context was saved and its cells freed while it waited (see
    /// [`LocalModels::waiting`]): its next [`LocalModels::acquire`] restores it before the call,
    /// so whoever wakes it (subagents done, a steer) needs nothing else.
    fn suspended(&self, _holder: &str) -> bool {
        false
    }
    /// Takes a slot on the model for one call, loading the model first if needed (waiting for
    /// room while the models in the way are in use, up to a limit the runtime sets) and queueing
    /// first-come, first-served while every slot is busy. `holder` names the agent asking, so the model can give it back the
    /// slot it used last, whose cached prompt is still there unless another agent has
    /// overwritten it (or restored from disk if the model was swapped out meanwhile).
    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a>;
    /// Like [`LocalModels::acquire`], for a call of the given priority. A runtime that does not
    /// rank calls serves them all alike.
    fn acquire_as<'a>(
        &'a self,
        model: &'a str,
        holder: &'a str,
        _priority: Priority,
    ) -> AcquireFuture<'a> {
        self.acquire(model, holder)
    }
    /// Like [`LocalModels::availability`], from the point of view of `session`: models other
    /// sessions are using are not available to it.
    fn availability_for(&self, model: &str, _session: &str) -> Availability {
        self.availability(model)
    }
    /// Like [`LocalModels::acquire_as`], with the agent's estimate of the call
    /// ([`LocalRequest`]): the runtime reserves tokens of the model's KV pool for it.
    fn acquire_with<'a>(
        &'a self,
        model: &'a str,
        holder: &'a str,
        request: LocalRequest,
    ) -> AcquireFuture<'a> {
        self.acquire_as(model, holder, request.priority)
    }
    /// Says that `holder` is working on `model` for one turn: while held, after the agent's
    /// first call on the loaded model, the model is never unloaded for another session's load,
    /// an idle timeout or a settings change. Held for the turn.
    fn attach(&self, _model: &str, _holder: &str) -> LocalPresence {
        LocalPresence::untracked()
    }
    /// Says that `holder` is asleep (waiting for its subagents, an approval, or a usage-limit
    /// pause). That releases its hold: the model may be swapped out for any load, its slots
    /// saved and restored. Held while it waits.
    fn waiting(&self, _model: &str, _holder: &str) -> LocalPresence {
        LocalPresence::untracked()
    }
    /// Reports one finished call to `model`: how many tokens it generated and how long
    /// generating them took (after the first token, or the whole call for a buffered response).
    /// The runtime keeps the measured speed ([`LocalModels::tokens_per_s`]).
    fn record_throughput(&self, _model: &str, _completion_tokens: u64, _generating: Duration) {}
    /// The generation speed measured for `model` so far, in tokens per second; `None` before
    /// any call finished. Slow models, such as one pinned to the CPU, are judged by it.
    fn tokens_per_s(&self, _model: &str) -> Option<f64> {
        None
    }
}
