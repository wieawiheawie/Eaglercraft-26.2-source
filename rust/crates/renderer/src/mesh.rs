use eagler_game_core::{BlockId, BlockRegistry};
use eagler_world::ChunkSection;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [i8; 3],
    pub block_id: u16,
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

/// Builds a section-local opaque mesh, omitting faces hidden by opaque neighbors.
pub fn mesh_section(
    section: &ChunkSection,
    neighbors: &SectionNeighbors<'_>,
    registry: &BlockRegistry,
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

                    let base = mesh.vertices.len() as u32;
                    for corner in face.corners {
                        mesh.vertices.push(Vertex {
                            position: [
                                f32::from(x) + corner[0],
                                f32::from(y) + corner[1],
                                f32::from(z) + corner[2],
                            ],
                            normal: face.normal,
                            block_id: id.0,
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
    use eagler_game_core::{BlockId, BlockRegistry};
    use eagler_world::ChunkSection;

    use crate::{mesh_section, SectionNeighbors};

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
}
