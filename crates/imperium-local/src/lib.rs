//! Local models: GGUF discovery, hardware probe, memory planning, runtime packs, and the
//! supervised llama.cpp servers that serve them.

pub mod gguf;
pub mod hardware;
pub mod planner;
pub mod process;
pub mod runtime;
pub mod scan;
pub mod supervisor;
