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
    _release: Box<dyn Send + Sync>,
}

impl LocalLease {
    /// `release` is dropped with the lease; its `Drop` frees the slot.
    pub fn new(base_url: String, slot: Option<u32>, release: impl Send + Sync + 'static) -> Self {
        LocalLease {
            base_url,
            slot,
            _release: Box::new(release),
        }
    }
}

impl fmt::Debug for LocalLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalLease")
            .field("base_url", &self.base_url)
            .field("slot", &self.slot)
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

pub type AcquireFuture<'a> = Pin<Box<dyn Future<Output = Result<LocalLease, String>> + Send + 'a>>;

/// Local models the router may place work on.
pub trait LocalModels: Send + Sync + fmt::Debug {
    /// Where the model with this id stands, without loading anything.
    fn availability(&self, model: &str) -> Availability;
    /// Takes a slot on the model for one call, loading the model first if needed (waiting for
    /// room while the models in the way are in use, up to a limit the runtime sets) and queueing
    /// first-come, first-served while every slot is busy. `holder` names the agent asking, so the model can give it back the
    /// slot it used last, whose cached prompt is still there unless another agent has
    /// overwritten it (or restored from disk if the model was swapped out meanwhile).
    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a>;
    /// Says that `holder` is working on `model` for one turn, so between its calls the model is
    /// not swapped out for another unless the other has waited long. Held for the turn.
    fn attach(&self, _model: &str, _holder: &str) -> LocalPresence {
        LocalPresence::untracked()
    }
    /// Says that `holder` is asleep waiting for its subagents, so its model may be swapped out
    /// for theirs. Held while it waits.
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
