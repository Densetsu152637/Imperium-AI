//! Clients for model endpoints. Every source speaks the OpenAI-compatible Chat Completions API,
//! so one streaming client covers hosted endpoints, llama.cpp, vLLM, Ollama, and LM Studio. The
//! agent loop reaches it through the [`ChatClient`] trait, built by endpoint kind from
//! [`ChatClients`], so clients for other APIs can be added.

mod chat;
mod client;
pub mod llamacpp;
mod sse;

pub use chat::{ChatRequest, Completion, Endpoint, ModelError, stream_chat};
pub use client::{
    ChatClient, ChatClientBuilder, ChatClients, ChatFuture, DEFAULT_KIND, LlamaCpp,
    OpenAiCompatible, TokenFuture, Tokenizer,
};
pub use imperium_api::{ChatMessage, FunctionCall, Pricing, ToolCall, ToolSpec, Usage};
