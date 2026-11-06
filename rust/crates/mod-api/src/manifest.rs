use std::collections::BTreeSet;

/// Versioned host capabilities a mod must explicitly request.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    RegisterBlocks,
    RegisterItems,
    RegisterRecipes,
    RegisterEntities,
    ListenGameplayEvents,
    ClientUi,
    ClientCommands,
    NetworkAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    InvalidId(String),
    InvalidVersion(String),
    ApiVersionMismatch { manifest: u32, client: u32 },
    CapabilityDenied(Capability),
}

/// Metadata and permissions declared by a separately distributed mod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModManifest {
    id: String,
    version: String,
    api_version: u32,
    capabilities: BTreeSet<Capability>,
}

impl ModManifest {
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        api_version: u32,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            api_version,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn api_version(&self) -> u32 {
        self.api_version
    }

    pub fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }

    pub fn allows(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn require(&self, capability: Capability) -> Result<(), ManifestError> {
        if self.allows(capability) {
            Ok(())
        } else {
            Err(ManifestError::CapabilityDenied(capability))
        }
    }

    pub fn validate_for(&self, client_api_version: u32) -> Result<(), ManifestError> {
        if !valid_mod_id(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        if !valid_semver(&self.version) {
            return Err(ManifestError::InvalidVersion(self.version.clone()));
        }
        if self.api_version != client_api_version {
            return Err(ManifestError::ApiVersionMismatch {
                manifest: self.api_version,
                client: client_api_version,
            });
        }
        Ok(())
    }
}

fn valid_mod_id(id: &str) -> bool {
    let Some((namespace, path)) = id.split_once(':') else {
        return false;
    };
    if namespace.is_empty()
        || path.is_empty()
        || id.matches(':').count() != 1
        || !namespace
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
        || !path
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c))
    {
        return false;
    }
    !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_semver(version: &str) -> bool {
    let without_build = version.split_once('+').map_or(version, |(core, _)| core);
    let core = without_build
        .split_once('-')
        .map_or(without_build, |(core, prerelease)| {
            if prerelease.is_empty()
                || !prerelease
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
            {
                return "";
            }
            core
        });
    let mut parts = core.split('.');
    let valid_number = |part: Option<&str>| {
        part.is_some_and(|value| {
            !value.is_empty()
                && value.bytes().all(|c| c.is_ascii_digit())
                && (value == "0" || !value.starts_with('0'))
                && value.parse::<u64>().is_ok()
        })
    };
    valid_number(parts.next()) && valid_number(parts.next()) && valid_number(parts.next()) && parts.next().is_none()
}

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
            assert!(
                matches!(
                    candidate.validate_for(1),
                    Err(ManifestError::InvalidId(_))
                ),
                "unexpectedly accepted id {id}"
            );
        }
    }

    #[test]
    fn rejects_invalid_semver_versions() {
        for version in ["1", "1.0", "one.two.three", "01.0.0", "1.-1.0", "1.0.0.0"] {
            let candidate = ModManifest::new("example:mod", version, 1, []);
            assert!(
                matches!(
                    candidate.validate_for(1),
                    Err(ManifestError::InvalidVersion(_))
                ),
                "unexpectedly accepted version {version}"
            );
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
