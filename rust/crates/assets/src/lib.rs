//! Resource locations, bounded asset caching, PNG decoding, Minecraft JSON
//! metadata parsing, and deterministic texture-atlas packing.

mod atlas;
mod json;
mod png_decode;
mod resource;

pub use atlas::{AtlasBuildError, AtlasRegion, TextureAtlas, TextureAtlasBuilder};
pub use json::{BlockModelJson, JsonAssetError, parse_block_model, parse_blockstate_model_ids};
pub use png_decode::{DecodedTexture, PngDecodeError, decode_png_rgba};
pub use resource::{AssetStore, ResourceError, ResourceLocation};
