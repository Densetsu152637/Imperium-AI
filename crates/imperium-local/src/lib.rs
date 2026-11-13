//! Local models: GGUF discovery, Hugging Face downloads, hardware probe, memory planning,
//! runtime packs, and the supervised llama.cpp and vLLM servers that serve them.

pub mod arbiter;
pub mod backend;
pub mod calibration;
pub mod card;
pub mod context;
pub mod gguf;
pub mod gpu;
pub mod hardware;
pub mod hf;
pub mod monitor;
pub mod planner;
pub mod process;
pub mod residency;
pub mod runtime;
pub mod runtime_check;
pub mod runtime_locations;
pub mod runtime_update;
pub mod scan;
pub(crate) mod slot_files;
pub mod speculative;
pub mod suggest;
pub mod supervisor;
