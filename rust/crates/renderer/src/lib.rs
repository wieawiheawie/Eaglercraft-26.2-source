//! Deterministic CPU mesh generation and the browser GPU renderer.

mod mesh;

pub use mesh::{BlockTextureSet, ChunkMesh, SectionNeighbors, Vertex, build_world_mesh, mesh_section, mesh_section_with_textures};

#[cfg(target_arch = "wasm32")]
mod gpu;

#[cfg(target_arch = "wasm32")]
pub use gpu::GpuRenderer;

mod queue;
pub use queue::{MeshJob, MeshJobQueue};
