use std::collections::BTreeMap;

/// Compact runtime identifier. Zero is permanently reserved for air.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct BlockId(pub u16);

impl BlockId {
    pub const AIR: Self = Self(0);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDefinition {
    pub id: BlockId,
    pub key: String,
    pub opaque: bool,
    pub solid: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    InvalidKey(String),
    DuplicateKey(String),
    IdExhausted,
}

#[derive(Clone, Debug)]
pub struct BlockRegistry {
    definitions: Vec<BlockDefinition>,
    by_key: BTreeMap<String, BlockId>,
    next_id: u32,
}

impl Default for BlockRegistry {
    fn default() -> Self { Self::new() }
}

impl BlockRegistry {
    pub fn new() -> Self {
        let air = BlockDefinition {
            id: BlockId::AIR,
            key: "minecraft:air".to_owned(),
            opaque: false,
            solid: false,
        };
        let mut by_key = BTreeMap::new();
        by_key.insert(air.key.clone(), BlockId::AIR);
        Self { definitions: vec![air], by_key, next_id: 1 }
    }

    pub fn register(&mut self, key: &str, opaque: bool, solid: bool) -> Result<BlockId, RegistryError> {
        if !valid_resource_key(key) {
            return Err(RegistryError::InvalidKey(key.to_owned()));
        }
        if self.by_key.contains_key(key) {
            return Err(RegistryError::DuplicateKey(key.to_owned()));
        }
        let id = u16::try_from(self.next_id).map(BlockId).map_err(|_| RegistryError::IdExhausted)?;
        let definition = BlockDefinition { id, key: key.to_owned(), opaque, solid };
        self.definitions.push(definition);
        self.by_key.insert(key.to_owned(), id);
        self.next_id += 1;
        Ok(id)
    }

    pub fn get(&self, id: BlockId) -> Option<&BlockDefinition> {
        self.definitions.get(usize::from(id.0))
    }

    pub fn id(&self, key: &str) -> Option<BlockId> {
        self.by_key.get(key).copied()
    }

    pub fn len(&self) -> usize { self.definitions.len() }
    pub fn is_empty(&self) -> bool { self.definitions.is_empty() }

    #[cfg(test)]
    pub(crate) fn set_next_id_for_test(&mut self, next_id: u32) {
        self.next_id = next_id;
    }
}

fn valid_resource_key(key: &str) -> bool {
    let Some((namespace, path)) = key.split_once(':') else { return false };
    if namespace.is_empty()
        || path.is_empty()
        || key.matches(':').count() != 1
        || !namespace.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
        || !path.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c))
    { return false; }
    !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::{BlockId, BlockRegistry, RegistryError};

    #[test]
    fn air_is_reserved_at_zero() {
        let registry = BlockRegistry::new();
        assert_eq!(registry.id("minecraft:air"), Some(BlockId::AIR));
        let air = registry.get(BlockId::AIR).expect("air must exist");
        assert!(!air.opaque);
        assert!(!air.solid);
    }

    #[test]
    fn registers_namespaced_block_keys_deterministically() {
        let mut registry = BlockRegistry::new();
        let first = registry.register("minecraft:stone", true, true).unwrap();
        let second = registry.register("minecraft:dirt", true, true).unwrap();
        assert_eq!(first.0, 1);
        assert_eq!(second.0, 2);
        assert_eq!(registry.id("minecraft:stone"), Some(first));
        assert_eq!(registry.get(second).unwrap().key, "minecraft:dirt");
    }

    #[test]
    fn rejects_unqualified_or_malformed_keys() {
        let mut registry = BlockRegistry::new();
        for key in ["stone", ":stone", "minecraft:", "Minecraft:stone", "minecraft:two words", "minecraft:a//b", "minecraft:../stone"] {
            assert!(matches!(registry.register(key, true, true), Err(RegistryError::InvalidKey(_))), "unexpectedly accepted {key}");
        }
    }

    #[test]
    fn rejects_duplicate_keys_without_changing_existing_id() {
        let mut registry = BlockRegistry::new();
        let id = registry.register("minecraft:stone", true, true).unwrap();
        assert!(matches!(registry.register("minecraft:stone", false, false), Err(RegistryError::DuplicateKey(_))));
        assert_eq!(registry.id("minecraft:stone"), Some(id));
        assert!(registry.get(id).unwrap().opaque);
    }

    #[test]
    fn refuses_to_allocate_an_id_outside_u16() {
        let mut registry = BlockRegistry::new();
        registry.set_next_id_for_test(u32::from(u16::MAX) + 1);
        assert_eq!(registry.register("example:overflow", true, true), Err(RegistryError::IdExhausted));
    }
}
