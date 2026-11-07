//! The interface between routing and the supervised local runtime (implemented by
//! `imperium-local`), so the agent loop can place work on local models without depending on how
//! they are loaded.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Whether a local model can take work right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Availability {
    /// Loaded with a free slot: reusing it costs nothing.
    Loaded,
    /// Not loaded (or every slot is busy) but fits in free memory: it can be loaded.
    Fits,
    /// Cannot run without evicting a model that is in use, or the model is unknown.
    Unavailable,
}

/// A slot on a loaded local model, held while an agent uses it. Dropping it frees the slot.
pub struct LocalLease {
    /// The OpenAI-compatible base URL of the model's server.
    pub base_url: String,
    _release: Box<dyn Send + Sync>,
}

impl LocalLease {
    /// `release` is dropped with the lease; its `Drop` frees the slot.
    pub fn new(base_url: String, release: impl Send + Sync + 'static) -> Self {
        LocalLease {
            base_url,
            _release: Box::new(release),
        }
    }
}

impl fmt::Debug for LocalLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalLease")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

pub type AcquireFuture<'a> = Pin<Box<dyn Future<Output = Result<LocalLease, String>> + Send + 'a>>;

/// Local models the router may place work on.
pub trait LocalModels: Send + Sync + fmt::Debug {
    /// Where the model with this id stands, without loading anything.
    fn availability(&self, model: &str) -> Availability;
    /// Takes a slot on the model, loading it first if needed.
    fn acquire<'a>(&'a self, model: &'a str) -> AcquireFuture<'a>;
}
