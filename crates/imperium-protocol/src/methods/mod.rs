//! Typed method definitions. Each method names its wire string and its params/result types.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ProtocolVersion;
use crate::session::PermissionDecision;

/// A daemon method: its wire name and the shapes of its params and result.
pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;
}

fn yes() -> bool {
    true
}

mod api;
mod attachments;
mod branching;
mod checkpoints;
mod config;
mod config_changes;
mod connectors;
mod environments;
mod eval;
mod export;
mod models;
mod mods;
mod pets;
mod phone;
mod plugins;
mod pull_requests;
mod resources;
mod review;
mod schedules;
mod session_control;
mod sessions;
mod system;

pub use api::*;
pub use attachments::*;
pub use branching::*;
pub use checkpoints::*;
pub use config::*;
pub use config_changes::*;
pub use connectors::*;
pub use environments::*;
pub use eval::*;
pub use export::*;
pub use models::*;
pub use mods::*;
pub use pets::*;
pub use phone::*;
pub use plugins::*;
pub use pull_requests::*;
pub use resources::*;
pub use review::*;
pub use schedules::*;
pub use session_control::*;
pub use sessions::*;
pub use system::*;
