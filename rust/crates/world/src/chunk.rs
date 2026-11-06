use std::collections::BTreeMap;

use eagler_game_core::BlockId;

pub const SECTION_EDGE: usize = 16;
pub const SECTION_VOLUME: usize = SECTION_EDGE * SECTION_EDGE * SECTION_EDGE;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkSection {
    blocks: Box<[BlockId; SECTION_VOLUME]>,
}

impl Default for ChunkSection {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkSection {
    pub fn new() -> Self {
        Self {
            blocks: Box::new([BlockId::AIR; SECTION_VOLUME]),
        }
    }

    pub fn get(&self, x: u8, y: u8, z: u8) -> BlockId {
        self.blocks[block_index(x, y, z)]
    }

    pub fn set(&mut self, x: u8, y: u8, z: u8, id: BlockId) -> BlockId {
        let slot = &mut self.blocks[block_index(x, y, z)];
        let previous = *slot;
        *slot = id;
        previous
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|id| *id == BlockId::AIR)
    }

    pub fn blocks(&self) -> &[BlockId; SECTION_VOLUME] {
        &self.blocks
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Chunk {
    sections: BTreeMap<i32, ChunkSection>,
}

impl Chunk {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_block(&self, x: u8, y: i32, z: u8) -> BlockId {
        let section_y = y.div_euclid(16);
        let local_y = y.rem_euclid(16) as u8;
        self.sections
            .get(&section_y)
            .map_or(BlockId::AIR, |section| section.get(x, local_y, z))
    }

    pub fn set_block(&mut self, x: u8, y: i32, z: u8, id: BlockId) -> BlockId {
        let section_y = y.div_euclid(16);
        let local_y = y.rem_euclid(16) as u8;

        if id == BlockId::AIR {
            let Some(section) = self.sections.get_mut(&section_y) else {
                return BlockId::AIR;
            };
            let previous = section.set(x, local_y, z, id);
            if section.is_empty() {
                self.sections.remove(&section_y);
            }
            previous
        } else {
            self.sections
                .entry(section_y)
                .or_default()
                .set(x, local_y, z, id)
        }
    }

    pub fn section(&self, section_y: i32) -> Option<&ChunkSection> {
        self.sections.get(&section_y)
    }

    pub fn section_mut(&mut self, section_y: i32) -> Option<&mut ChunkSection> {
        self.sections.get_mut(&section_y)
    }

    pub fn sections(&self) -> impl Iterator<Item = (i32, &ChunkSection)> {
        self.sections.iter().map(|(y, section)| (*y, section))
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

fn block_index(x: u8, y: u8, z: u8) -> usize {
    assert!(usize::from(x) < SECTION_EDGE, "local x is outside a section");
    assert!(usize::from(y) < SECTION_EDGE, "local y is outside a section");
    assert!(usize::from(z) < SECTION_EDGE, "local z is outside a section");
    (usize::from(y) * SECTION_EDGE + usize::from(z)) * SECTION_EDGE + usize::from(x)
}
