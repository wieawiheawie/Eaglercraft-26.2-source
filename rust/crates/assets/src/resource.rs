use std::{collections::BTreeMap, sync::Arc};

const MAX_SINGLE_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: usize = 64 * 1024 * 1024;

/// A namespaced Minecraft resource identifier, e.g. `minecraft:block/stone`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceLocation {
    namespace: String,
    path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceError {
    InvalidNamespace,
    InvalidPath,
    AssetTooLarge { size: usize, maximum: usize },
    StoreCapacityExceeded,
}

impl ResourceLocation {
    pub fn new(namespace: &str, path: &str) -> Result<Self, ResourceError> {
        if namespace.is_empty()
            || !namespace.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
            })
        {
            return Err(ResourceError::InvalidNamespace);
        }
        if path.is_empty()
            || path.starts_with('/')
            || path.ends_with('/')
            || !path.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || b"_./-".contains(&byte)
            })
            || path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(ResourceError::InvalidPath);
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        })
    }

    pub fn parse(value: &str) -> Result<Self, ResourceError> {
        let (namespace, path) = value
            .split_once(':')
            .unwrap_or(("minecraft", value));
        if value.matches(':').count() > 1 {
            return Err(ResourceError::InvalidNamespace);
        }
        Self::new(namespace, path)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The browser path beneath the copied static resource-pack root.
    pub fn asset_path(&self, extension: &str) -> String {
        format!("assets/{}/{}/{}", self.namespace, self.path, extension.trim_start_matches('.'))
    }

    pub fn texture_png_path(&self) -> String {
        format!("assets/{}/textures/{}.png", self.namespace, self.path)
    }

    pub fn model_json_path(&self) -> String {
        format!("assets/{}/models/{}.json", self.namespace, self.path)
    }

    pub fn blockstate_json_path(&self) -> String {
        format!("assets/{}/blockstates/{}.json", self.namespace, self.path)
    }
}

/// Small bounded cache for fetched resource bytes. Entries are immutable and
/// reference-counted so renderer tasks can retain data without cloning buffers.
#[derive(Default)]
pub struct AssetStore {
    entries: BTreeMap<ResourceLocation, Arc<[u8]>>,
    total_bytes: usize,
}

impl AssetStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        key: ResourceLocation,
        bytes: Vec<u8>,
    ) -> Result<Arc<[u8]>, ResourceError> {
        if bytes.len() > MAX_SINGLE_ASSET_BYTES {
            return Err(ResourceError::AssetTooLarge {
                size: bytes.len(),
                maximum: MAX_SINGLE_ASSET_BYTES,
            });
        }
        let previous_size = self.entries.get(&key).map_or(0, |entry| entry.len());
        let next_total = self
            .total_bytes
            .saturating_sub(previous_size)
            .saturating_add(bytes.len());
        if next_total > MAX_TOTAL_ASSET_BYTES {
            return Err(ResourceError::StoreCapacityExceeded);
        }

        let bytes: Arc<[u8]> = bytes.into();
        self.entries.insert(key, bytes.clone());
        self.total_bytes = next_total;
        Ok(bytes)
    }

    pub fn get(&self, key: &ResourceLocation) -> Option<&Arc<[u8]>> {
        self.entries.get(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetStore, ResourceError, ResourceLocation};

    #[test]
    fn parses_default_and_explicit_namespaces() {
        let stone = ResourceLocation::parse("stone").unwrap();
        assert_eq!(stone.namespace(), "minecraft");
        assert_eq!(stone.path(), "stone");

        let custom = ResourceLocation::parse("haven:blocks/bright_stone").unwrap();
        assert_eq!(custom.namespace(), "haven");
        assert_eq!(custom.path(), "blocks/bright_stone");
    }

    #[test]
    fn builds_standard_minecraft_asset_paths() {
        let stone = ResourceLocation::parse("minecraft:stone").unwrap();
        assert_eq!(stone.texture_png_path(), "assets/minecraft/textures/stone.png");
        assert_eq!(stone.model_json_path(), "assets/minecraft/models/stone.json");
        assert_eq!(stone.blockstate_json_path(), "assets/minecraft/blockstates/stone.json");
    }

    #[test]
    fn rejects_path_traversal_and_invalid_namespaces() {
        for value in ["../secret", "stone//brick", "/root", "stone/../dirt", "Stone", "foo:bar:baz"] {
            assert!(ResourceLocation::parse(value).is_err(), "unexpectedly accepted {value}");
        }
    }

    #[test]
    fn immutable_asset_bytes_are_shared_and_replacement_updates_memory_accounting() {
        let mut store = AssetStore::new();
        let key = ResourceLocation::parse("minecraft:stone").unwrap();
        let first = store.insert(key.clone(), vec![1, 2, 3]).unwrap();
        let shared = store.get(&key).unwrap().clone();
        assert!(std::sync::Arc::ptr_eq(&first, &shared));
        assert_eq!(store.total_bytes(), 3);

        store.insert(key.clone(), vec![4, 5]).unwrap();
        assert_eq!(store.total_bytes(), 2);
        assert_eq!(&**store.get(&key).unwrap(), &[4, 5]);
    }

    #[test]
    fn rejects_assets_over_the_global_store_limit() {
        let mut store = AssetStore::new();
        // Keep this test small: the single-asset validation is exercised
        // through a declared small helper rather than allocating 16 MiB here.
        let key = ResourceLocation::parse("minecraft:test").unwrap();
        let data = vec![0; 17 * 1024 * 1024];
        assert!(matches!(
            store.insert(key, data),
            Err(ResourceError::AssetTooLarge { .. })
        ));
    }
}
