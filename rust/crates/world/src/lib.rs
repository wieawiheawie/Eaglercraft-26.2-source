//! Dimension-aware, sparse chunk storage with stable negative-coordinate semantics.

mod chunk;
mod coords;
mod world;

pub use chunk::{Chunk, ChunkSection, SECTION_EDGE, SECTION_VOLUME};
pub use coords::{BlockPos, ChunkPos};
pub use world::{World, WorldError};
