//! Deterministic CPU mesh generation; GPU submission lives behind the browser renderer.

mod mesh;

pub use mesh::{ChunkMesh, SectionNeighbors, Vertex, mesh_section};
