//! Clients for model endpoints. Every source speaks the OpenAI-compatible Chat Completions API,
//! so one streaming client covers hosted endpoints, llama.cpp, vLLM, Ollama, and LM Studio.

mod chat;
pub mod llamacpp;
mod pricing;
mod sse;

pub use chat::{
    ChatMessage, ChatRequest, Completion, Endpoint, FunctionCall, ModelError, ToolCall, ToolSpec,
    Usage, stream_chat,
};
pub use pricing::Pricing;
