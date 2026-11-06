//! Deterministic, platform-independent game rules and registries.

mod block;

pub use block::{BlockDefinition, BlockId, BlockRegistry, RegistryError};

mod lighting;
mod math;

pub use lighting::propagate_light_16;
pub use math::{Aabb, Plane};

mod player;
pub use player::{Player, PlayerInput};
