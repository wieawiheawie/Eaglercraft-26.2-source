use std::hash::Hash;

pub const SECTION_EDGE: i32 = 16;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn chunk(self) -> ChunkPos {
        ChunkPos {
            x: self.x.div_euclid(SECTION_EDGE),
            z: self.z.div_euclid(SECTION_EDGE),
        }
    }

    pub fn section_y(self) -> i32 {
        self.y.div_euclid(SECTION_EDGE)
    }

    pub fn local(self) -> [u8; 3] {
        [
            self.x.rem_euclid(SECTION_EDGE) as u8,
            self.y.rem_euclid(SECTION_EDGE) as u8,
            self.z.rem_euclid(SECTION_EDGE) as u8,
        ]
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    pub fn origin(self) -> BlockPos {
        BlockPos::new(self.x * SECTION_EDGE, 0, self.z * SECTION_EDGE)
    }
}
