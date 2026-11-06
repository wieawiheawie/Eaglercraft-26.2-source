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
        for key in [
            "stone",
            ":stone",
            "minecraft:",
            "Minecraft:stone",
            "minecraft:two words",
            "minecraft:a//b",
            "minecraft:../stone",
        ] {
            assert!(matches!(
                registry.register(key, true, true),
                Err(RegistryError::InvalidKey(_))
            ), "unexpectedly accepted {key}");
        }
    }

    #[test]
    fn rejects_duplicate_keys_without_changing_existing_id() {
        let mut registry = BlockRegistry::new();
        let id = registry.register("minecraft:stone", true, true).unwrap();
        assert!(matches!(
            registry.register("minecraft:stone", false, false),
            Err(RegistryError::DuplicateKey(_))
        ));
        assert_eq!(registry.id("minecraft:stone"), Some(id));
        assert_eq!(registry.get(id).unwrap().opaque, true);
    }

    #[test]
    fn refuses_to_allocate_an_id_outside_u16() {
        let mut registry = BlockRegistry::new();
        registry.set_next_id_for_test(u32::from(u16::MAX) + 1);
        assert_eq!(
            registry.register("example:overflow", true, true),
            Err(RegistryError::IdExhausted)
        );
    }
}
