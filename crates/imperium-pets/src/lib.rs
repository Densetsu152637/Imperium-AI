//! Pixel Pets v3 in Imperium: packages and the behaviour arbiter that turns agent activity
//! into animations. Pets never call a model.

pub mod arbiter;
pub mod package;

pub use arbiter::{Arbiter, PetEvent, Shown};
pub use package::{Animation, Pet, PetError, discover};
