#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Capability, ManifestError, ModManifest};

    fn manifest() -> ModManifest {
        ModManifest::new(
            "example:example_mod",
            "1.2.3",
            1,
            [
                Capability::RegisterBlocks,
                Capability::RegisterItems,
                Capability::ListenGameplayEvents,
            ],
        )
    }

    #[test]
    fn accepts_a_valid_namespaced_manifest() {
        assert_eq!(manifest().validate_for(1), Ok(()));
    }

    #[test]
    fn rejects_a_manifest_for_another_api_version() {
        assert_eq!(
            manifest().validate_for(2),
            Err(ManifestError::ApiVersionMismatch {
                manifest: 1,
                client: 2,
            })
        );
    }

    #[test]
    fn rejects_invalid_mod_identifiers() {
        for id in [
            "example",
            ":mod",
            "example:",
            "Example:mod",
            "example:two words",
            "example:../mod",
            "example:a//b",
        ] {
            let candidate = ModManifest::new(id, "1.0.0", 1, []);
            assert!(matches!(
                candidate.validate_for(1),
                Err(ManifestError::InvalidId(_))
            ), "unexpectedly accepted id {id}");
        }
    }

    #[test]
    fn rejects_invalid_semver_versions() {
        for version in ["1", "1.0", "one.two.three", "01.0.0", "1.-1.0", "1.0.0.0"] {
            let candidate = ModManifest::new("example:mod", version, 1, []);
            assert!(matches!(
                candidate.validate_for(1),
                Err(ManifestError::InvalidVersion(_))
            ), "unexpectedly accepted version {version}");
        }
    }

    #[test]
    fn capabilities_are_explicit_and_deduplicated() {
        let mod_manifest = manifest();
        assert!(mod_manifest.allows(Capability::RegisterBlocks));
        assert!(!mod_manifest.allows(Capability::NetworkAccess));
        assert_eq!(
            mod_manifest.capabilities(),
            &BTreeSet::from([
                Capability::RegisterBlocks,
                Capability::RegisterItems,
                Capability::ListenGameplayEvents,
            ])
        );
        assert_eq!(
            mod_manifest.require(Capability::NetworkAccess),
            Err(ManifestError::CapabilityDenied(Capability::NetworkAccess))
        );
    }
}
