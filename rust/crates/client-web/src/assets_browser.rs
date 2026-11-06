use eagler_assets::{
    decode_png_rgba, AssetStore, ResourceLocation, TextureAtlasBuilder,
};
use eagler_renderer::GpuRenderer;
use js_sys::Uint8Array;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// Fetches one file from the static resource pack copied beside index.html.
async fn fetch_resource(path: &str) -> Result<Vec<u8>, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("Browser window is unavailable"))?;
    let url = format!("./{path}");
    let response = JsFuture::from(window.fetch_with_str(&url))
        .await?
        .dyn_into::<web_sys::Response>()?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "Resource request failed (HTTP {}) for {path}",
            response.status()
        )));
    }
    let buffer = JsFuture::from(response.array_buffer()?).await?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// Loads genuine Minecraft resource-pack textures, decodes them in Rust,
/// packs a deterministic RGBA atlas, and uploads the atlas to the GPU.
pub async fn load_starter_texture_atlas(renderer: &mut GpuRenderer) -> Result<usize, JsValue> {
    let resources = [
        "minecraft:block/stone",
        "minecraft:block/dirt",
        "minecraft:block/grass_block_top",
        "minecraft:block/grass_block_side",
        "minecraft:block/oak_planks",
    ];

    let mut cache = AssetStore::new();
    let mut builder = TextureAtlasBuilder::new(256, 256, 2);
    for identifier in resources {
        let key = ResourceLocation::parse(identifier)
            .map_err(|error| JsValue::from_str(&format!("Invalid built-in texture id: {error:?}")))?;
        let path = key.texture_png_path();
        let bytes = fetch_resource(&path).await?;
        cache
            .insert(key.clone(), bytes.clone())
            .map_err(|error| JsValue::from_str(&format!("Could not cache {identifier}: {error:?}")))?;
        let decoded = decode_png_rgba(&bytes)
            .map_err(|error| JsValue::from_str(&format!("Could not decode {path}: {error:?}")))?;
        builder
            .insert(key, decoded)
            .map_err(|error| JsValue::from_str(&format!("Could not pack {identifier}: {error:?}")))?;
    }

    let atlas = builder
        .build()
        .map_err(|error| JsValue::from_str(&format!("Could not build Minecraft texture atlas: {error:?}")))?;
    let loaded = atlas.regions().len();
    renderer
        .upload_texture_atlas(&atlas)
        .map_err(|error| JsValue::from_str(&error))?;

    // Cache is intentionally bounded and local to startup. The GPU owns the
    // retained atlas from this point; individual source PNGs can be discarded.
    cache.clear();
    Ok(loaded)
}
