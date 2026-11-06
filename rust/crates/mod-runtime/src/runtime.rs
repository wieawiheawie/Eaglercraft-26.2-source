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
