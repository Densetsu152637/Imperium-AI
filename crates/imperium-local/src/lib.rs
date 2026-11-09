//! Local models: GGUF discovery, Hugging Face downloads, hardware probe, memory planning,
//! runtime packs, and the supervised llama.cpp and vLLM servers that serve them.

pub mod backend;
pub mod gguf;
pub mod hardware;
pub mod hf;
pub mod monitor;
pub mod planner;
pub mod process;
pub mod runtime;
pub mod scan;
pub mod suggest;
pub mod supervisor;
