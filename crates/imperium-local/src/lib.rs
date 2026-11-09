//! Local models: GGUF discovery, Hugging Face downloads, hardware probe, memory planning,
//! runtime packs, and the supervised llama.cpp and vLLM servers that serve them.

pub(crate) mod arbiter;
pub mod backend;
pub mod gguf;
pub mod hardware;
pub mod hf;
pub mod monitor;
pub mod planner;
pub mod process;
pub mod runtime;
pub mod runtime_check;
pub mod runtime_locations;
pub mod runtime_update;
pub mod scan;
pub(crate) mod slot_files;
pub mod suggest;
pub mod supervisor;
