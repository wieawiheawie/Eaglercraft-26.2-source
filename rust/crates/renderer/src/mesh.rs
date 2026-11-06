use std::collections::BTreeMap;

use eagler_assets::AtlasRegion;
use eagler_game_core::{BlockId, BlockRegistry};
use eagler_world::{ChunkPos, ChunkSection, World};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [i8; 3],
    pub block_id: u16,
    /// Absolute normalized coordinates within the resource texture atlas.
    pub uv: [f32; 2],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SectionNeighbors<'a> {
    pub above: Option<&'a ChunkSection>,
    pub below: Option<&'a ChunkSection>,
    pub north: Option<&'a ChunkSection>,
    pub south: Option<&'a ChunkSection>,
    pub east: Option<&'a ChunkSection>,
    pub west: Option<&'a ChunkSection>,
}

/// Per-face atlas selection for a block. Blocks such as grass can use
/// different top, bottom and side textures without special cases in the GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockTextureSet {
    pub side: AtlasRegion,
    pub top: AtlasRegion,
    pub bottom: AtlasRegion,
}

impl BlockTextureSet {
    pub const fn uniform(region: AtlasRegion) -> Self {
        Self {
            side: region,
            top: region,
            bottom: region,
        }
    }

    fn for_normal(self, normal: [i8; 3]) -> AtlasRegion {
        if normal[1] > 0 {
            self.top
        } else if normal[1] < 0 {
            self.bottom
        } else {
            self.side
        }
    }
}

#[derive(Clone, Copy)]
struct Face {
    normal: [i8; 3],
    offset: [i32; 3],
    corners: [[f32; 3]; 4],
}

const FACES: [Face; 6] = [
    Face { normal: [1, 0, 0], offset: [1, 0, 0], corners: [[1.0,0.0,0.0],[1.0,1.0,0.0],[1.0,1.0,1.0],[1.0,0.0,1.0]] },
    Face { normal: [-1, 0, 0], offset: [-1, 0, 0], corners: [[0.0,0.0,0.0],[0.0,0.0,1.0],[0.0,1.0,1.0],[0.0,1.0,0.0]] },
    Face { normal: [0, 1, 0], offset: [0, 1, 0], corners: [[0.0,1.0,0.0],[0.0,1.0,1.0],[1.0,1.0,1.0],[1.0,1.0,0.0]] },
    Face { normal: [0, -1, 0], offset: [0, -1, 0], corners: [[0.0,0.0,0.0],[1.0,0.0,0.0],[1.0,0.0,1.0],[0.0,0.0,1.0]] },
    Face { normal: [0, 0, 1], offset: [0, 0, 1], corners: [[0.0,0.0,1.0],[1.0,0.0,1.0],[1.0,1.0,1.0],[0.0,1.0,1.0]] },
    Face { normal: [0, 0, -1], offset: [0, 0, -1], corners: [[0.0,0.0,0.0],[0.0,1.0,0.0],[1.0,1.0,0.0],[1.0,0.0,0.0]] },
];

const FACE_UVS: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

/// Builds an opaque section mesh using the full texture atlas when no block
/// texture table is supplied. This entry point is useful for mesh tests.
pub fn mesh_section(
    section: &ChunkSection,
    neighbors: &SectionNeighbors<'_>,
    registry: &BlockRegistry,
) -> ChunkMesh {
    mesh_section_with_textures(section, neighbors, registry, &BTreeMap::new())
}

pub fn mesh_section_with_textures(
    section: &ChunkSection,
    neighbors: &SectionNeighbors<'_>,
    registry: &BlockRegistry,
    textures: &BTreeMap<BlockId, BlockTextureSet>,
) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();

    for y in 0..16_u8 {
        for z in 0..16_u8 {
            for x in 0..16_u8 {
                let id = section.get(x, y, z);
                if id == BlockId::AIR {
                    continue;
                }
                let Some(block) = registry.get(id) else {
                    continue;
                };

                for face in FACES {
                    let neighbor_id = sample_neighbor(
                        section,
                        neighbors,
                        i32::from(x) + face.offset[0],
                        i32::from(y) + face.offset[1],
                        i32::from(z) + face.offset[2],
                    );
                    if registry.get(neighbor_id).is_some_and(|neighbor| neighbor.opaque) {
                        continue;
                    }

                    let region = textures.get(&id).copied().map(|set| set.for_normal(face.normal));
                    let uv_min = region.map_or([0.0, 0.0], AtlasRegion::uv_min);
                    let uv_max = region.map_or([1.0, 1.0], AtlasRegion::uv_max);
                    let base = mesh.vertices.len() as u32;
                    for (corner, local_uv) in face.corners.into_iter().zip(FACE_UVS) {
                        mesh.vertices.push(Vertex {
                            position: [
                                f32::from(x) + corner[0],
                                f32::from(y) + corner[1],
                                f32::from(z) + corner[2],
                            ],
                            normal: face.normal,
                            block_id: id.0,
                            uv: [
                                uv_min[0] + (uv_max[0] - uv_min[0]) * local_uv[0],
                                uv_min[1] + (uv_max[1] - uv_min[1]) * local_uv[1],
                            ],
                        });
                    }
                    mesh.indices
                        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                }
            }
        }
    }
    mesh
}

/// Builds one aggregate world mesh from the currently loaded sparse chunks.
/// Neighbor samples are taken across chunk boundaries before section geometry
/// is merged into reusable world-space vertex/index vectors.
pub fn build_world_mesh(
    world: &World,
    registry: &BlockRegistry,
    textures: &BTreeMap<BlockId, BlockTextureSet>,
) -> ChunkMesh {
    let mut result = ChunkMesh::default();
    for (chunk_pos, chunk) in world.chunks() {
        for (section_y, section) in chunk.sections() {
            let neighbors = SectionNeighbors {
                above: chunk.section(section_y.saturating_add(1)),
                below: chunk.section(section_y.saturating_sub(1)),
                north: world
                    .chunk(ChunkPos::new(chunk_pos.x, chunk_pos.z.saturating_sub(1)))
                    .and_then(|neighbor| neighbor.section(section_y)),
                south: world
                    .chunk(ChunkPos::new(chunk_pos.x, chunk_pos.z.saturating_add(1)))
                    .and_then(|neighbor| neighbor.section(section_y)),
                east: world
                    .chunk(ChunkPos::new(chunk_pos.x.saturating_add(1), chunk_pos.z))
                    .and_then(|neighbor| neighbor.section(section_y)),
                west: world
                    .chunk(ChunkPos::new(chunk_pos.x.saturating_sub(1), chunk_pos.z))
                    .and_then(|neighbor| neighbor.section(section_y)),
            };
            let section_mesh = mesh_section_with_textures(section, &neighbors, registry, textures);
            let vertex_base = result.vertices.len() as u32;
            let world_x = chunk_pos.x as f32 * 16.0;
            let world_y = section_y as f32 * 16.0;
            let world_z = chunk_pos.z as f32 * 16.0;
            result.vertices.extend(section_mesh.vertices.into_iter().map(|mut vertex| {
                vertex.position[0] += world_x;
                vertex.position[1] += world_y;
                vertex.position[2] += world_z;
                vertex
            }));
            result
                .indices
                .extend(section_mesh.indices.into_iter().map(|index| index + vertex_base));
        }
    }
    result
}

fn sample_neighbor(
    current: &ChunkSection,
    neighbors: &SectionNeighbors<'_>,
    x: i32,
    y: i32,
    z: i32,
) -> BlockId {
    if (0..16).contains(&x) && (0..16).contains(&y) && (0..16).contains(&z) {
        return current.get(x as u8, y as u8, z as u8);
    }
    let (section, sample_x, sample_y, sample_z) = if x < 0 {
        (neighbors.west, 15, y, z)
    } else if x >= 16 {
        (neighbors.east, 0, y, z)
    } else if y < 0 {
        (neighbors.below, x, 15, z)
    } else if y >= 16 {
        (neighbors.above, x, 0, z)
    } else if z < 0 {
        (neighbors.north, x, y, 15)
    } else {
        (neighbors.south, x, y, 0)
    };

    section.map_or(BlockId::AIR, |section| {
        section.get(sample_x as u8, sample_y as u8, sample_z as u8)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use eagler_game_core::{BlockId, BlockRegistry};
    use eagler_world::{BlockPos, ChunkSection, World};

    use crate::{build_world_mesh, mesh_section, SectionNeighbors};

    fn registry() -> BlockRegistry {
        let mut registry = BlockRegistry::new();
        registry.register("minecraft:stone", true, true).unwrap();
        registry
    }

    #[test]
    fn one_opaque_cube_emits_six_faces() {
        let registry = registry();
        let mut section = ChunkSection::new();
        section.set(1, 1, 1, BlockId(1));
        let mesh = mesh_section(&section, &SectionNeighbors::default(), &registry);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
    }

    #[test]
    fn adjacent_opaque_blocks_hide_the_shared_face() {
        let registry = registry();
        let mut section = ChunkSection::new();
        section.set(1, 1, 1, BlockId(1));
        section.set(2, 1, 1, BlockId(1));
        let mesh = mesh_section(&section, &SectionNeighbors::default(), &registry);
        assert_eq!(mesh.vertices.len(), 40);
        assert_eq!(mesh.indices.len(), 60);
    }

    #[test]
    fn empty_section_emits_no_geometry() {
        let registry = registry();
        let mesh = mesh_section(
            &ChunkSection::new(),
            &SectionNeighbors::default(),
            &registry,
        );
        assert!(mesh.vertices.is_empty());
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn neighboring_section_hides_chunk_boundary_face() {
        let registry = registry();
        let mut section = ChunkSection::new();
        let mut east = ChunkSection::new();
        section.set(15, 4, 8, BlockId(1));
        east.set(0, 4, 8, BlockId(1));
        let neighbors = SectionNeighbors {
            east: Some(&east),
            ..SectionNeighbors::default()
        };
        let mesh = mesh_section(&section, &neighbors, &registry);
        assert_eq!(mesh.vertices.len(), 20);
        assert_eq!(mesh.indices.len(), 30);
    }

    #[test]
    fn world_builder_culls_faces_between_adjacent_chunks() {
        let mut world = World::new(-64, 384).unwrap();
        world.set_block(BlockPos::new(15, 1, 0), BlockId(1)).unwrap();
        world.set_block(BlockPos::new(16, 1, 0), BlockId(1)).unwrap();
        let mesh = build_world_mesh(&world, &registry(), &BTreeMap::new());
        assert_eq!(mesh.vertices.len(), 40);
        assert_eq!(mesh.indices.len(), 60);
        assert_eq!(mesh.vertices[0].position[0], 15.0);
        assert_eq!(mesh.vertices[20].position[0], 16.0);
    }
}
