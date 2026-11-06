//! Borrowed-slice decoder for the Minecraft 26.2 chunk-with-light play packet.
//!
//! Frame bodies are parsed directly from the login stream buffer. Only the
//! decoded block-state arrays are allocated; NBT, palette storage, light data,
//! and packet framing are skipped in place with strict bounds checks.

const MAX_CHUNK_DATA_BYTES: usize = 2 * 1024 * 1024;
const OVERWORLD_SECTION_COUNT: usize = 24;
const OVERWORLD_MIN_SECTION_Y: i32 = -4;
const MAX_PALETTE_ENTRIES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkDecodeError {
    Truncated,
    InvalidVarInt,
    InvalidLength,
    PacketTooLarge(usize),
    InvalidPalette,
    InvalidNbt,
    TrailingBytes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedSection {
    pub section_y: i32,
    /// Global Minecraft block-state registry IDs, in Y/Z/X storage order.
    pub block_states: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedChunk {
    pub x: i32,
    pub z: i32,
    pub min_section_y: i32,
    pub sections: Vec<DecodedSection>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorldPacket {
    Chunk(DecodedChunk),
    ForgetChunk { x: i32, z: i32 },
    PlayerPosition {
        teleport_id: i32,
        position: [f64; 3],
        delta_movement: [f64; 3],
        yaw: f32,
        pitch: f32,
        relative_flags: i32,
    },
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ChunkDecodeError> {
        let end = self.offset.checked_add(count).ok_or(ChunkDecodeError::InvalidLength)?;
        let slice = self.bytes.get(self.offset..end).ok_or(ChunkDecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ChunkDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ChunkDecodeError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("two-byte slice")))
    }

    fn i16(&mut self) -> Result<i16, ChunkDecodeError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().expect("two-byte slice")))
    }

    fn i32(&mut self) -> Result<i32, ChunkDecodeError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().expect("four-byte slice")))
    }

    fn varint(&mut self) -> Result<i32, ChunkDecodeError> {
        let mut value = 0_u32;
        for index in 0..5 {
            let byte = self.u8()?;
            value |= u32::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(ChunkDecodeError::InvalidVarInt)
    }

    fn count(&mut self, maximum: usize) -> Result<usize, ChunkDecodeError> {
        let value = self.varint()?;
        if value < 0 {
            return Err(ChunkDecodeError::InvalidLength);
        }
        let value = value as usize;
        if value > maximum {
            return Err(ChunkDecodeError::PacketTooLarge(value));
        }
        Ok(value)
    }

    fn skip_varint_string(&mut self, maximum: usize) -> Result<(), ChunkDecodeError> {
        let length = self.count(maximum)?;
        self.take(length)?;
        Ok(())
    }

    fn long_array(&mut self, maximum: usize) -> Result<(), ChunkDecodeError> {
        let count = self.count(maximum)?;
        let byte_count = count.checked_mul(8).ok_or(ChunkDecodeError::InvalidLength)?;
        self.take(byte_count)?;
        Ok(())
    }

    fn byte_array(&mut self, maximum: usize) -> Result<(), ChunkDecodeError> {
        let length = self.count(maximum)?;
        self.take(length)?;
        Ok(())
    }

    fn read_block_palette(&mut self) -> Result<Vec<u32>, ChunkDecodeError> {
        let bits = self.u8()? as usize;
        if bits == 0 {
            // ZeroBitStorage serializes one palette entry and no data array.
            let state = self.varint()?;
            if state < 0 {
                return Err(ChunkDecodeError::InvalidPalette);
            }
            return Ok(vec![state as u32; 4096]);
        }
        if bits > 31 {
            return Err(ChunkDecodeError::InvalidPalette);
        }

        let palette = if bits <= 8 {
            let count = self.count(MAX_PALETTE_ENTRIES)?;
            if count == 0 {
                return Err(ChunkDecodeError::InvalidPalette);
            }
            let mut palette = Vec::with_capacity(count);
            for _ in 0..count {
                let state = self.varint()?;
                if state < 0 {
                    return Err(ChunkDecodeError::InvalidPalette);
                }
                palette.push(state as u32);
            }
            Some(palette)
        } else {
            None
        };

        // PalettedContainer writes its backing storage as a fixed-size long
        // array: unlike LongArray NBT and light masks, it has no VarInt count.
        let values_per_long = 64 / bits;
        let expected_longs = 4096_usize.div_ceil(values_per_long);
        let mut packed = Vec::with_capacity(expected_longs);
        for _ in 0..expected_longs {
            packed.push(u64::from_be_bytes(self.take(8)?.try_into().expect("eight-byte long")));
        }

        let mask = (1_u64 << bits) - 1;
        let mut states = Vec::with_capacity(4096);
        for index in 0..4096 {
            let word = packed[index / values_per_long];
            let shift = (index % values_per_long) * bits;
            let value = ((word >> shift) & mask) as u32;
            let state = match &palette {
                Some(entries) => *entries.get(value as usize).ok_or(ChunkDecodeError::InvalidPalette)?,
                None => value,
            };
            states.push(state);
        }
        Ok(states)
    }

    fn skip_biome_palette(&mut self) -> Result<(), ChunkDecodeError> {
        let bits = self.u8()? as usize;
        if bits == 0 {
            let value = self.varint()?;
            if value < 0 {
                return Err(ChunkDecodeError::InvalidPalette);
            }
            return Ok(());
        }
        if bits > 31 {
            return Err(ChunkDecodeError::InvalidPalette);
        }
        if bits <= 3 {
            let count = self.count(64)?;
            if count == 0 {
                return Err(ChunkDecodeError::InvalidPalette);
            }
            for _ in 0..count {
                if self.varint()? < 0 {
                    return Err(ChunkDecodeError::InvalidPalette);
                }
            }
        }
        let values_per_long = 64 / bits;
        let expected = 64_usize.div_ceil(values_per_long);
        self.take(expected.checked_mul(8).ok_or(ChunkDecodeError::InvalidLength)?)?;
        Ok(())
    }

    fn skip_nbt(&mut self) -> Result<(), ChunkDecodeError> {
        let tag_type = self.u8()?;
        if tag_type == 0 {
            return Ok(());
        }
        self.skip_nbt_string()?;
        self.skip_nbt_payload(tag_type, 0)
    }

    fn skip_nbt_string(&mut self) -> Result<(), ChunkDecodeError> {
        let length = self.u16()? as usize;
        self.take(length)?;
        Ok(())
    }

    fn skip_nbt_payload(&mut self, tag_type: u8, depth: usize) -> Result<(), ChunkDecodeError> {
        if depth > 64 {
            return Err(ChunkDecodeError::InvalidNbt);
        }
        match tag_type {
            1 => { self.take(1)?; }
            2 => { self.take(2)?; }
            3 | 5 => { self.take(4)?; }
            4 | 6 => { self.take(8)?; }
            7 => {
                let length = self.i32()?;
                if !(0..=MAX_CHUNK_DATA_BYTES as i32).contains(&length) { return Err(ChunkDecodeError::InvalidNbt); }
                self.take(length as usize)?;
            }
            8 => self.skip_nbt_string()?,
            9 => {
                let element_type = self.u8()?;
                let length = self.i32()?;
                if !(0..=MAX_PALETTE_ENTRIES as i32).contains(&length) || (length > 0 && element_type == 0) {
                    return Err(ChunkDecodeError::InvalidNbt);
                }
                for _ in 0..length {
                    self.skip_nbt_payload(element_type, depth + 1)?;
                }
            }
            10 => loop {
                let child_type = self.u8()?;
                if child_type == 0 { break; }
                self.skip_nbt_string()?;
                self.skip_nbt_payload(child_type, depth + 1)?;
            },
            11 => {
                let length = self.i32()?;
                if !(0..=MAX_PALETTE_ENTRIES as i32).contains(&length) { return Err(ChunkDecodeError::InvalidNbt); }
                self.take((length as usize).checked_mul(4).ok_or(ChunkDecodeError::InvalidNbt)?)?;
            }
            12 => {
                let length = self.i32()?;
                if !(0..=MAX_PALETTE_ENTRIES as i32).contains(&length) { return Err(ChunkDecodeError::InvalidNbt); }
                self.take((length as usize).checked_mul(8).ok_or(ChunkDecodeError::InvalidNbt)?)?;
            }
            _ => return Err(ChunkDecodeError::InvalidNbt),
        }
        Ok(())
    }

    fn skip_bit_set(&mut self) -> Result<(), ChunkDecodeError> {
        self.long_array(256)
    }

    fn skip_light_arrays(&mut self) -> Result<(), ChunkDecodeError> {
        let count = self.count(256)?;
        for _ in 0..count {
            self.byte_array(2048)?;
        }
        Ok(())
    }
}

/// Decode the protocol-776 clientbound level-chunk-with-light packet (play ID 44).
/// The current world storage is the 384-block overworld (-64..320), i.e. 24 sections.
pub fn decode_chunk_with_light(payload: &[u8]) -> Result<DecodedChunk, ChunkDecodeError> {
    let mut cursor = Cursor::new(payload);
    let x = cursor.i32()?;
    let z = cursor.i32()?;

    // Heightmap enum keys and long arrays.
    let heightmap_count = cursor.count(256)?;
    for _ in 0..heightmap_count {
        if cursor.varint()? < 0 {
            return Err(ChunkDecodeError::InvalidLength);
        }
        cursor.long_array(4096)?;
    }

    let chunk_data_len = cursor.count(MAX_CHUNK_DATA_BYTES)?;
    let chunk_data = cursor.take(chunk_data_len)?;
    let mut sections_cursor = Cursor::new(chunk_data);
    let mut sections = Vec::with_capacity(OVERWORLD_SECTION_COUNT);
    for index in 0..OVERWORLD_SECTION_COUNT {
        let _non_empty_blocks = sections_cursor.i16()?;
        let _non_empty_fluids = sections_cursor.i16()?;
        let block_states = sections_cursor.read_block_palette()?;
        sections_cursor.skip_biome_palette()?;
        sections.push(DecodedSection {
            section_y: OVERWORLD_MIN_SECTION_Y + index as i32,
            block_states,
        });
    }
    if !sections_cursor.is_empty() {
        return Err(ChunkDecodeError::TrailingBytes);
    }

    // Block entity records: packed X/Z byte, Y short, type registry VarInt, NBT.
    let block_entities = cursor.count(4096)?;
    for _ in 0..block_entities {
        cursor.u8()?;
        cursor.i16()?;
        if cursor.varint()? < 0 {
            return Err(ChunkDecodeError::InvalidLength);
        }
        cursor.skip_nbt()?;
    }

    // Four BitSet masks followed by sky and block light-array lists.
    cursor.skip_bit_set()?;
    cursor.skip_bit_set()?;
    cursor.skip_bit_set()?;
    cursor.skip_bit_set()?;
    cursor.skip_light_arrays()?;
    cursor.skip_light_arrays()?;

    if !cursor.is_empty() {
        return Err(ChunkDecodeError::TrailingBytes);
    }

    Ok(DecodedChunk {
        x,
        z,
        min_section_y: OVERWORLD_MIN_SECTION_Y,
        sections,
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_chunk_with_light, ChunkDecodeError, OVERWORLD_SECTION_COUNT};

    fn varint(mut value: i32, out: &mut Vec<u8>) {
        loop {
            if value & !0x7f == 0 { out.push(value as u8); return; }
            out.push(((value & 0x7f) | 0x80) as u8);
            value = ((value as u32) >> 7) as i32;
        }
    }

    fn empty_overworld_chunk(x: i32, z: i32) -> Vec<u8> {
        let mut section_data = Vec::new();
        for _ in 0..OVERWORLD_SECTION_COUNT {
            section_data.extend_from_slice(&0_i16.to_be_bytes());
            section_data.extend_from_slice(&0_i16.to_be_bytes());
            section_data.push(0); varint(0, &mut section_data); // Uniform air block palette.
            section_data.push(0); varint(0, &mut section_data); // Uniform biome palette.
        }

        let mut packet = Vec::new();
        packet.extend_from_slice(&x.to_be_bytes());
        packet.extend_from_slice(&z.to_be_bytes());
        varint(0, &mut packet); // heightmaps
        varint(section_data.len() as i32, &mut packet);
        packet.extend_from_slice(&section_data);
        varint(0, &mut packet); // block entities
        for _ in 0..4 { varint(0, &mut packet); } // light masks
        varint(0, &mut packet); // sky updates
        varint(0, &mut packet); // block updates
        packet
    }

    #[test]
    fn decodes_all_sections_and_light_tail_without_copying_packet_frames() {
        let packet = empty_overworld_chunk(3, -2);
        let decoded = decode_chunk_with_light(&packet).unwrap();
        assert_eq!((decoded.x, decoded.z), (3, -2));
        assert_eq!(decoded.min_section_y, -4);
        assert_eq!(decoded.sections.len(), 24);
        assert_eq!(decoded.sections[0].block_states.len(), 4096);
        assert!(decoded.sections.iter().all(|section| section.block_states.iter().all(|id| *id == 0)));
    }

    #[test]
    fn decodes_local_palette_using_fixed_size_packed_longs() {
        let mut section_data = Vec::new();
        section_data.extend_from_slice(&0_i16.to_be_bytes());
        section_data.extend_from_slice(&0_i16.to_be_bytes());
        section_data.push(4); // Four bits per block-state palette index.
        varint(2, &mut section_data);
        varint(0, &mut section_data); // Air.
        varint(1, &mut section_data); // Stone.
        for index in 0..256 {
            let word = if index == 0 { 1_u64 } else { 0_u64 };
            section_data.extend_from_slice(&word.to_be_bytes());
        }
        section_data.push(0);
        varint(0, &mut section_data); // Uniform biome palette.

        for _ in 1..OVERWORLD_SECTION_COUNT {
            section_data.extend_from_slice(&0_i16.to_be_bytes());
            section_data.extend_from_slice(&0_i16.to_be_bytes());
            section_data.push(0); varint(0, &mut section_data);
            section_data.push(0); varint(0, &mut section_data);
        }

        let mut packet = Vec::new();
        packet.extend_from_slice(&4_i32.to_be_bytes());
        packet.extend_from_slice(&(-7_i32).to_be_bytes());
        varint(0, &mut packet); // Heightmaps.
        varint(section_data.len() as i32, &mut packet);
        packet.extend_from_slice(&section_data);
        varint(0, &mut packet); // Block entities.
        for _ in 0..4 { varint(0, &mut packet); } // Light masks.
        varint(0, &mut packet); // Sky light arrays.
        varint(0, &mut packet); // Block light arrays.

        let decoded = decode_chunk_with_light(&packet).unwrap();
        assert_eq!((decoded.x, decoded.z), (4, -7));
        assert_eq!(decoded.sections[0].block_states[0], 1);
        assert!(decoded.sections[0].block_states[1..].iter().all(|id| *id == 0));
    }

    #[test]
    fn rejects_truncated_light_or_chunk_data() {
        let mut packet = empty_overworld_chunk(0, 0);
        packet.pop();
        assert!(matches!(decode_chunk_with_light(&packet), Err(ChunkDecodeError::Truncated)));
    }
}
