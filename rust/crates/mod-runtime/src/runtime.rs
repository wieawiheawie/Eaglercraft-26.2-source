use eagler_mod_api::{ManifestError, ModManifest};
use wasmi::{
    Config, Engine, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc, Val,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLimits {
    pub fuel_per_call: u64,
    pub max_memory_bytes: usize,
    pub max_module_bytes: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            fuel_per_call: 1_000_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_module_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
struct ModHostState {
    limits: StoreLimits,
}

#[derive(Debug)]
pub enum ModLoadError {
    InvalidManifest(ManifestError),
    ModuleTooLarge { size: usize, limit: usize },
    InvalidWasm(String),
    Instantiation(String),
    MissingTickExport(String),
    FuelConfiguration(String),
}

#[derive(Debug)]
pub enum ModExecutionError {
    FuelConfiguration(String),
    Trap(String),
}

/// A reusable Wasmi engine configured for isolated mod instances.
#[derive(Debug)]
pub struct ModRuntime {
    api_version: u32,
    limits: RuntimeLimits,
    engine: Engine,
}

impl ModRuntime {
    pub fn new(api_version: u32, limits: RuntimeLimits) -> Self {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self {
            api_version,
            limits,
            engine,
        }
    }

    /// Validates the manifest and creates an isolated Wasmi store for one mod.
    ///
    /// No WASI or ambient host imports are linked. A module can only import
    /// functions deliberately added to this host in a later, capability-checked
    /// ABI revision.
    pub fn load(
        &self,
        manifest: &ModManifest,
        wasm_bytes: &[u8],
    ) -> Result<LoadedMod, ModLoadError> {
        manifest
            .validate_for(self.api_version)
            .map_err(ModLoadError::InvalidManifest)?;

        if wasm_bytes.len() > self.limits.max_module_bytes {
            return Err(ModLoadError::ModuleTooLarge {
                size: wasm_bytes.len(),
                limit: self.limits.max_module_bytes,
            });
        }

        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|error| ModLoadError::InvalidWasm(error.to_string()))?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .instances(1)
            .memories(1)
            .tables(1)
            .build();
        let state = ModHostState { limits };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);

        // Deliberately empty: filesystem, networking, clock, and other imports
        // are not available unless a later ABI explicitly grants them.
        let linker = Linker::<ModHostState>::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|error| ModLoadError::Instantiation(error.to_string()))?;
        let on_tick = instance
            .get_typed_func::<(), ()>(&store, "on_tick")
            .map_err(|error| ModLoadError::MissingTickExport(error.to_string()))?;

        store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|error| ModLoadError::FuelConfiguration(error.to_string()))?;

        Ok(LoadedMod {
            manifest: manifest.clone(),
            instance,
            store,
            on_tick,
            fuel_per_call: self.limits.fuel_per_call,
        })
    }
}

/// A single mod instance. Each loaded mod owns its own Wasmi store and limits.
#[derive(Debug)]
pub struct LoadedMod {
    manifest: ModManifest,
    instance: Instance,
    store: Store<ModHostState>,
    on_tick: TypedFunc<(), ()>,
    fuel_per_call: u64,
}

impl LoadedMod {
    pub fn manifest(&self) -> &ModManifest {
        &self.manifest
    }

    /// Executes one tick with a fresh deterministic fuel budget.
    pub fn tick(&mut self) -> Result<(), ModExecutionError> {
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|error| ModExecutionError::FuelConfiguration(error.to_string()))?;
        self.on_tick
            .call(&mut self.store, ())
            .map_err(|error| ModExecutionError::Trap(error.to_string()))
    }

    /// Reads an exported i32 global for diagnostics and test probes.
    pub fn read_i32_export(&self, name: &str) -> Option<i32> {
        let global = self.instance.get_global(&self.store, name)?;
        match global.get(&self.store) {
            Val::I32(value) => Some(value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use eagler_mod_api::{Capability, ModManifest};

    use super::{ModExecutionError, ModLoadError, ModRuntime, RuntimeLimits};

    fn manifest() -> ModManifest {
        ModManifest::new("example:test_mod", "1.0.0", 1, [Capability::RegisterBlocks])
    }

    fn runtime(fuel: u64, memory: usize, max_module: usize) -> ModRuntime {
        ModRuntime::new(
            1,
            RuntimeLimits {
                fuel_per_call: fuel,
                max_memory_bytes: memory,
                max_module_bytes: max_module,
            },
        )
    }

    fn wasm(source: &str) -> Vec<u8> {
        wat::parse_str(source).expect("test WAT should compile")
    }

    #[test]
    fn runs_the_mod_tick_export_and_preserves_its_state() {
        let bytes = wasm(
            r#"(module
                (global (export "ticks") (mut i32) (i32.const 0))
                (func (export "on_tick")
                    (global.set 0 (i32.add (global.get 0) (i32.const 1)))
                )
            )"#,
        );
        let mut loaded = runtime(10_000, 64 * 1024, 1024 * 1024)
            .load(&manifest(), &bytes)
            .unwrap();

        loaded.tick().unwrap();
        loaded.tick().unwrap();

        assert_eq!(loaded.read_i32_export("ticks"), Some(2));
    }

    #[test]
    fn rejects_an_incompatible_mod_api_before_instantiation() {
        let incompatible = ModManifest::new("example:old_mod", "1.0.0", 9, []);
        assert!(matches!(
            runtime(1000, 64 * 1024, 1024).load(&incompatible, &wasm("(module)")),
            Err(ModLoadError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_modules_above_the_configured_file_size() {
        let bytes = wasm("(module)");
        assert!(matches!(
            runtime(1000, 64 * 1024, 4).load(&manifest(), &bytes),
            Err(ModLoadError::ModuleTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_memory_above_the_per_mod_limit() {
        let bytes = wasm("(module (memory (export \"memory\") 2))");
        assert!(matches!(
            runtime(1000, 64 * 1024, 1024).load(&manifest(), &bytes),
            Err(ModLoadError::Instantiation(_))
        ));
    }

    #[test]
    fn does_not_grant_unknown_host_imports() {
        let bytes = wasm(
            r#"(module
                (import "host_fs" "read_file" (func))
                (func (export "on_tick"))
            )"#,
        );
        assert!(matches!(
            runtime(1000, 64 * 1024, 1024).load(&manifest(), &bytes),
            Err(ModLoadError::Instantiation(_))
        ));
    }

    #[test]
    fn fuel_exhaustion_traps_the_call() {
        let bytes = wasm(
            r#"(module
                (func (export "on_tick")
                    (loop $forever (br $forever))
                )
            )"#,
        );
        let mut loaded = runtime(100, 64 * 1024, 1024).load(&manifest(), &bytes).unwrap();
        assert!(matches!(loaded.tick(), Err(ModExecutionError::Trap(_))));
    }

    #[test]
    fn a_trapping_mod_does_not_poison_another_mod_instance() {
        let bad = wasm(
            r#"(module
                (func (export "on_tick") (unreachable))
            )"#,
        );
        let good = wasm(
            r#"(module
                (global (export "ticks") (mut i32) (i32.const 0))
                (func (export "on_tick")
                    (global.set 0 (i32.add (global.get 0) (i32.const 1)))
                )
            )"#,
        );
        let engine = runtime(1000, 64 * 1024, 1024);
        let mut first = engine.load(&manifest(), &bad).unwrap();
        let mut second = engine.load(&manifest(), &good).unwrap();

        assert!(first.tick().is_err());
        second.tick().unwrap();

        assert_eq!(second.read_i32_export("ticks"), Some(1));
    }
}
