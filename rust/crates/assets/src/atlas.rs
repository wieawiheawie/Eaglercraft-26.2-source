use std::collections::BTreeMap;

use crate::{DecodedTexture, ResourceLocation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub atlas_width: u32,
    pub atlas_height: u32,
}

impl AtlasRegion {
    pub fn uv_min(self) -> [f32; 2] {
        [
            self.x as f32 / self.atlas_width as f32,
            self.y as f32 / self.atlas_height as f32,
        ]
    }

    pub fn uv_max(self) -> [f32; 2] {
        [
            (self.x + self.width) as f32 / self.atlas_width as f32,
            (self.y + self.height) as f32 / self.atlas_height as f32,
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AtlasBuildError {
    InvalidDimensions,
    TextureTooLarge(ResourceLocation),
    AtlasFull(ResourceLocation),
    PixelBufferOverflow,
}

#[derive(Clone, Debug)]
pub struct TextureAtlas {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 texture data.
    pub pixels: Vec<u8>,
    regions: BTreeMap<ResourceLocation, AtlasRegion>,
}

impl TextureAtlas {
    pub fn region(&self, key: &ResourceLocation) -> Option<AtlasRegion> {
        self.regions.get(key).copied()
    }

    pub fn regions(&self) -> &BTreeMap<ResourceLocation, AtlasRegion> {
        &self.regions
    }
}

#[derive(Default)]
pub struct TextureAtlasBuilder {
    width: u32,
    height: u32,
    padding: u32,
    textures: BTreeMap<ResourceLocation, DecodedTexture>,
}

impl TextureAtlasBuilder {
    pub fn new(width: u32, height: u32, padding: u32) -> Self {
        Self {
            width,
            height,
            padding,
            textures: BTreeMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        key: ResourceLocation,
        texture: DecodedTexture,
    ) -> Result<(), AtlasBuildError> {
        if self.width == 0 || self.height == 0 {
            return Err(AtlasBuildError::InvalidDimensions);
        }
        let expected = (texture.width as usize)
            .checked_mul(texture.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(AtlasBuildError::PixelBufferOverflow)?;
        if texture.width == 0 || texture.height == 0 || texture.pixels.len() != expected {
            return Err(AtlasBuildError::PixelBufferOverflow);
        }
        self.textures.insert(key, texture);
        Ok(())
    }

    /// Deterministic shelf packing in resource-key sort order with edge padding
    /// extruded from each texture to avoid bilinear sampling seams.
    pub fn build(self) -> Result<TextureAtlas, AtlasBuildError> {
        if self.width == 0 || self.height == 0 {
            return Err(AtlasBuildError::InvalidDimensions);
        }
        let atlas_len = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(AtlasBuildError::PixelBufferOverflow)?;
        let mut pixels = vec![0; atlas_len];
        let mut regions = BTreeMap::new();

        let mut x = 0_u32;
        let mut y = 0_u32;
        let mut shelf_height = 0_u32;
        for (key, texture) in self.textures {
            let padded_width = texture
                .width
                .checked_add(self.padding.saturating_mul(2))
                .ok_or(AtlasBuildError::PixelBufferOverflow)?;
            let padded_height = texture
                .height
                .checked_add(self.padding.saturating_mul(2))
                .ok_or(AtlasBuildError::PixelBufferOverflow)?;
            if padded_width > self.width || padded_height > self.height {
                return Err(AtlasBuildError::TextureTooLarge(key));
            }
            if x.checked_add(padded_width).is_none_or(|next| next > self.width) {
                x = 0;
                y = y
                    .checked_add(shelf_height)
                    .ok_or(AtlasBuildError::PixelBufferOverflow)?;
                shelf_height = 0;
            }
            if y.checked_add(padded_height).is_none_or(|next| next > self.height) {
                return Err(AtlasBuildError::AtlasFull(key));
            }

            let region = AtlasRegion {
                x: x + self.padding,
                y: y + self.padding,
                width: texture.width,
                height: texture.height,
                atlas_width: self.width,
                atlas_height: self.height,
            };
            blit_with_extrusion(
                &mut pixels,
                self.width,
                self.height,
                self.padding,
                region,
                &texture,
            )?;
            regions.insert(key, region);
            x += padded_width;
            shelf_height = shelf_height.max(padded_height);
        }

        Ok(TextureAtlas {
            width: self.width,
            height: self.height,
            pixels,
            regions,
        })
    }
}

fn blit_with_extrusion(
    destination: &mut [u8],
    atlas_width: u32,
    atlas_height: u32,
    padding: u32,
    region: AtlasRegion,
    texture: &DecodedTexture,
) -> Result<(), AtlasBuildError> {
    let start_x = region.x.saturating_sub(padding);
    let start_y = region.y.saturating_sub(padding);
    let end_x = (region.x + region.width + padding).min(atlas_width);
    let end_y = (region.y + region.height + padding).min(atlas_height);

    for destination_y in start_y..end_y {
        for destination_x in start_x..end_x {
            let source_x = destination_x.clamp(region.x, region.x + region.width - 1) - region.x;
            let source_y = destination_y.clamp(region.y, region.y + region.height - 1) - region.y;
            let source_index = ((source_y * texture.width + source_x) * 4) as usize;
            let destination_index = ((destination_y * atlas_width + destination_x) * 4) as usize;
            let source_pixel = texture
                .pixels
                .get(source_index..source_index + 4)
                .ok_or(AtlasBuildError::PixelBufferOverflow)?;
            destination
                .get_mut(destination_index..destination_index + 4)
                .ok_or(AtlasBuildError::PixelBufferOverflow)?
                .copy_from_slice(source_pixel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{DecodedTexture, ResourceLocation, TextureAtlasBuilder};

    fn texture(width: u32, height: u32, rgba: [u8; 4]) -> DecodedTexture {
        let mut pixels = Vec::new();
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&rgba);
        }
        DecodedTexture { width, height, pixels }
    }

    #[test]
    fn packs_multiple_textures_and_exposes_normalized_uvs() {
        let stone = ResourceLocation::parse("minecraft:stone").unwrap();
        let dirt = ResourceLocation::parse("minecraft:dirt").unwrap();
        let mut builder = TextureAtlasBuilder::new(8, 4, 1);
        builder.insert(stone.clone(), texture(2, 2, [255, 0, 0, 255])).unwrap();
        builder.insert(dirt.clone(), texture(2, 2, [0, 255, 0, 255])).unwrap();

        let atlas = builder.build().unwrap();
        assert_eq!(atlas.regions().len(), 2);
        assert_ne!(atlas.region(&stone), atlas.region(&dirt));
        assert!(atlas.region(&stone).unwrap().uv_max()[0] <= 1.0);
        assert_eq!(atlas.pixels.len(), 8 * 4 * 4);
    }

    #[test]
    fn padding_extrudes_edge_pixels() {
        let key = ResourceLocation::parse("minecraft:stone").unwrap();
        let mut builder = TextureAtlasBuilder::new(4, 4, 1);
        builder.insert(key.clone(), texture(2, 2, [90, 80, 70, 255])).unwrap();
        let atlas = builder.build().unwrap();
        // The first padded texel at the atlas origin repeats the source edge.
        assert_eq!(&atlas.pixels[..4], &[90, 80, 70, 255]);
    }

    #[test]
    fn rejects_atlas_overflow() {
        let first = ResourceLocation::parse("minecraft:first").unwrap();
        let second = ResourceLocation::parse("minecraft:second").unwrap();
        let mut builder = TextureAtlasBuilder::new(4, 4, 1);
        builder.insert(first, texture(2, 2, [1, 2, 3, 4])).unwrap();
        builder.insert(second, texture(2, 2, [5, 6, 7, 8])).unwrap();
        assert!(builder.build().is_err());
    }
}
