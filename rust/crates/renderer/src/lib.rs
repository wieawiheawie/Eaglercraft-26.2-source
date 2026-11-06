//! Deterministic CPU mesh generation and the browser GPU renderer.

mod mesh;

pub use mesh::{ChunkMesh, SectionNeighbors, Vertex, mesh_section};

#[cfg(target_arch = "wasm32")]
mod gpu;

#[cfg(target_arch = "wasm32")]
pub use gpu::GpuRenderer;
