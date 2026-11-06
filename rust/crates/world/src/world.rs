use std::collections::BTreeMap;

use eagler_game_core::BlockId;

use crate::{BlockPos, Chunk, ChunkPos};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorldError {
    InvalidHeight,
    OutsideDimension { y: i32, min_y: i32, max_y_exclusive: i32 },
}

#[derive(Clone, Debug)]
pub struct World {
    min_y: i32,
    max_y_exclusive: i32,
    chunks: BTreeMap<ChunkPos, Chunk>,
}

impl World {
    pub fn new(min_y: i32, height: u32) -> Result<Self, WorldError> {
        if height == 0 || height > i32::MAX as u32 {
            return Err(WorldError::InvalidHeight);
        }
        let max_y_exclusive = min_y.checked_add(height as i32).ok_or(WorldError::InvalidHeight)?;
        Ok(Self {
            min_y,
            max_y_exclusive,
            chunks: BTreeMap::new(),
        })
    }

    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    pub fn max_y_exclusive(&self) -> i32 {
        self.max_y_exclusive
    }

    pub fn get_block(&self, pos: BlockPos) -> Result<BlockId, WorldError> {
        self.check_y(pos.y)?;
        let chunk_pos = pos.chunk();
        let [local_x, local_y, local_z] = pos.local();
        Ok(self
            .chunks
            .get(&chunk_pos)
            .map_or(BlockId::AIR, |chunk| {
                chunk.get_block(local_x, pos.y, local_z)
            }))
    }

    pub fn set_block(&mut self, pos: BlockPos, id: BlockId) -> Result<BlockId, WorldError> {
        self.check_y(pos.y)?;
        let chunk_pos = pos.chunk();
        if id == BlockId::AIR && !self.chunks.contains_key(&chunk_pos) {
            return Ok(BlockId::AIR);
        }
        let [local_x, _local_y, local_z] = pos.local();
        let chunk = self.chunks.entry(chunk_pos).or_default();
        let previous = chunk.set_block(local_x, pos.y, local_z, id);
        if chunk.is_empty() {
            self.chunks.remove(&chunk_pos);
        }
        Ok(previous)
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    pub fn chunks(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        self.chunks.iter().map(|(pos, chunk)| (*pos, chunk))
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    fn check_y(&self, y: i32) -> Result<(), WorldError> {
        if (self.min_y..self.max_y_exclusive).contains(&y) {
            Ok(())
        } else {
            Err(WorldError::OutsideDimension {
                y,
                min_y: self.min_y,
                max_y_exclusive: self.max_y_exclusive,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use eagler_game_core::BlockId;

    use crate::{BlockPos, ChunkPos, World, WorldError};

    #[test]
    fn negative_coordinates_use_euclidean_chunk_and_local_positions() {
        assert_eq!(BlockPos::new(-1, -1, -17).chunk(), ChunkPos::new(-1, -2));
        assert_eq!(BlockPos::new(-1, -1, -17).local(), [15, 15, 15]);
        assert_eq!(BlockPos::new(-16, -16, -16).chunk(), ChunkPos::new(-1, -1));
        assert_eq!(BlockPos::new(-16, -16, -16).local(), [0, 0, 0]);
    }

    #[test]
    fn setting_air_in_an_absent_chunk_does_not_allocate() {
        let mut world = World::new(-64, 384).unwrap();
        assert_eq!(world.set_block(BlockPos::new(-30, 0, 70), BlockId::AIR), Ok(BlockId::AIR));
        assert_eq!(world.chunk_count(), 0);
    }

    #[test]
    fn blocks_round_trip_across_negative_chunk_boundaries() {
        let mut world = World::new(-64, 384).unwrap();
        let stone = BlockId(1);
        let pos = BlockPos::new(-17, -1, -16);
        assert_eq!(world.set_block(pos, stone), Ok(BlockId::AIR));
        assert_eq!(world.get_block(pos), Ok(stone));
        assert_eq!(world.chunk_count(), 1);
        assert_eq!(world.set_block(pos, BlockId::AIR), Ok(stone));
        assert_eq!(world.chunk_count(), 0);
    }

    #[test]
    fn rejects_coordinates_outside_the_dimension() {
        let mut world = World::new(-64, 384).unwrap();
        assert!(matches!(
            world.set_block(BlockPos::new(0, 320, 0), BlockId(1)),
            Err(WorldError::OutsideDimension { y: 320, .. })
        ));
        assert!(matches!(
            world.get_block(BlockPos::new(0, -65, 0)),
            Err(WorldError::OutsideDimension { y: -65, .. })
        ));
        assert_eq!(world.chunk_count(), 0);
    }

    #[test]
    fn rejects_zero_or_overflowing_dimension_heights() {
        assert_eq!(World::new(0, 0).err(), Some(WorldError::InvalidHeight));
        assert_eq!(World::new(i32::MAX, 2).err(), Some(WorldError::InvalidHeight));
    }
}
