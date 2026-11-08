//! Clients for model endpoints. Every source speaks the OpenAI-compatible Chat Completions API,
//! so one streaming client covers hosted endpoints, llama.cpp, vLLM, Ollama, and LM Studio.

mod chat;
pub mod llamacpp;
mod sse;

pub use chat::{ChatRequest, Completion, Endpoint, ModelError, stream_chat};
pub use imperium_api::{ChatMessage, FunctionCall, Pricing, ToolCall, ToolSpec, Usage};
