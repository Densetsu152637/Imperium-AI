//! Models: Ratio's model names to Imperium's chat clients and embedder, with the card
//! admission reads.

use std::sync::Arc;

use imperium_memory::retrieval::Embedder;
use imperium_models::ChatClient;
use ratio::frame::{Card, Location};

/// A model a Ratio program may name.
#[derive(Clone)]
pub struct ModelEntry {
    /// The name programs use (`"local:qwen"`, a configured name).
    pub name: String,
    /// The model id the endpoint takes.
    pub model_id: String,
    pub client: Arc<dyn ChatClient>,
    pub location: Location,
    pub card: Card,
}

impl std::fmt::Debug for ModelEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEntry")
            .field("name", &self.name)
            .field("model_id", &self.model_id)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

/// The session's embedding model (`memory.embedding_model`).
#[derive(Clone)]
pub struct EmbedderEntry {
    pub embedder: Arc<dyn Embedder>,
    pub location: Location,
}

/// Imperium's estimate where it has no tokenizer answer (four characters a token, as the
/// engine's context breakdowns estimate). The exact count needs the model server's
/// `/tokenize`, which is asynchronous; `Host::count_tokens` is synchronous.
pub fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count())
        .unwrap_or(u64::MAX)
        .div_ceil(4)
}
