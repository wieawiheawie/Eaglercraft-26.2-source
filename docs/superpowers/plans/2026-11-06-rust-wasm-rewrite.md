# Eaglercraft 26.2 Rust/Wasm Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Java/TeaVM browser game with a Rust-dominated Minecraft 26.2-compatible browser client, sandboxed Rust/Wasm mod system, and a reproducible low-memory build.

**Architecture:** A new Cargo workspace under `rust/` owns game logic, chunks/world, CPU meshing, renderer, browser integration, network/persistence and mod runtime. Use `wgpu` for WebGPU with WebGL2 fallback, and a narrow generated browser-binding layer; retain Java source as a compatibility oracle and reference until the Rust client demonstrates feature parity.

**Tech Stack:** Rust stable, `wasm32-unknown-unknown`, Cargo, `wasm-bindgen`, `web-sys`, `wgpu`, `wasmi`, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-06-rust-wasm-rewrite-design.md`

## Global Constraints

- 8 GiB is the absolute maximum build RAM; aim for less and reserve memory for the OS.
- Release browser compile and packaging must finish in no more than 20 minutes on an 8 GiB reference runner; faster is better.
- Default build starts with two Cargo jobs, no LTO, stripped release symbols, and moderate optimization.
- Rust owns the gameplay/runtime/rendering/network/world/save/modding logic; Java can only be a temporary bridge, compatibility oracle, and migration reference.
- Generated JavaScript bindings are allowed only as browser interop glue; no JavaScript game engine/gameplay.
- WebGPU is preferred; WebGL2 fallback is required for older Chromebook browsers.
- Mod modules are sandboxed, capability-limited, versioned `.wasm` plugins; no native dynamic libraries.
- Keep existing Java and TeaVM sources untouched until Rust parity gates pass.
- Do not claim performance or the memory/time gates pass without measured CI evidence.

## Review Focus

- Negative world coordinates must map to the correct chunk and local block coordinates.
- Invalid block identifiers and out-of-dimension Y coordinates must not corrupt world state.
- A mesh must not emit faces between two opaque neighboring blocks, including chunk boundaries.
- WebGL2-only browsers must not be treated as WebGPU-capable; initialization must fail with a useful error if neither backend is available.
- A malicious or runaway mod must not gain arbitrary filesystem/network access or consume unbounded execution time.

---

## Task 1: Rust workspace and constrained build harness

**Files:**
- Create: `rust/Cargo.toml`
- Create: `rust/rust-toolchain.toml`
- Create: `rust/.cargo/config.toml`
- Create: `rust/README.md`
- Create: `.github/workflows/rust-client.yml`
- Create: `rust/scripts/measure-build.sh`

**Interfaces:**
- Workspace members will be `crates/game-core`, `crates/world`, `crates/renderer`, `crates/mod-api`, `crates/mod-runtime`, and `crates/client-web`.
- Default browser build command: `./scripts/build-web.sh` from `rust/`.
- CI checks native tests and a wasm32 release build, records duration and peak RSS, and enforces a 20-minute job timeout.

- [ ] Create workspace manifest with resolver 3 and the six named members.
- [ ] Pin stable Rust and the `wasm32-unknown-unknown` target.
- [ ] Add memory-conscious profiles: release opt-level 2, no LTO, 16 codegen units, stripped symbols, panic abort; dev symbols disabled for the web target.
- [ ] Add a build measurement script that prints elapsed seconds and GNU time peak RSS in MiB and exits nonzero above 7,168 MiB or 1,200 seconds.
- [ ] Add CI with `CARGO_BUILD_JOBS=2`, a 20-minute timeout, `cargo test --workspace`, and a measured `cargo build --release --target wasm32-unknown-unknown -p client-web`.
- [ ] Verify workflow syntax and inspect the first CI run before calling the build gate complete.

## Task 2: Core identifiers and registry

**Files:**
- Create: `rust/crates/game-core/Cargo.toml`
- Create: `rust/crates/game-core/src/lib.rs`
- Create: `rust/crates/game-core/src/block.rs`
- Test: `rust/crates/game-core/src/block.rs` unit tests

**Interfaces:**
- `BlockId(u16)`, with `BlockId::AIR` equal to zero.
- `BlockDefinition { id: BlockId, key: String, opaque: bool, solid: bool }`.
- `BlockRegistry::register(key: &str, opaque: bool, solid: bool) -> Result<BlockId, RegistryError>`.
- `BlockRegistry::get(id: BlockId) -> Option<&BlockDefinition>`; `BlockRegistry::id(key: &str) -> Option<BlockId>`.

- [ ] Write tests for air reservation, namespaced-key validation, duplicate rejection, ID lookup, and exhaustion.
- [ ] Run tests and confirm they fail because the registry does not exist.
- [ ] Implement the registry with deterministic sequential IDs and a reverse key map.
- [ ] Run `cargo test -p game-core` and the full workspace suite; commit.

## Task 3: World coordinates, sections and chunks

**Files:**
- Create: `rust/crates/world/Cargo.toml`
- Create: `rust/crates/world/src/lib.rs`
- Create: `rust/crates/world/src/coords.rs`
- Create: `rust/crates/world/src/chunk.rs`
- Create: `rust/crates/world/src/world.rs`

**Interfaces:**
- `BlockPos { x: i32, y: i32, z: i32 }`.
- `ChunkPos { x: i32, z: i32 }`; conversion uses Euclidean division/remainder so negative coordinates work.
- `ChunkSection` stores exactly 4,096 `BlockId` values in Y-major local indexing.
- `Chunk` contains sections keyed by signed section Y; setting air in an absent section does not allocate it.
- `World::set_block(BlockPos, BlockId)` and `World::get_block(BlockPos) -> BlockId`.

- [ ] Write tests for section indexing, all six edges, negative X/Z coordinates, absent chunks, and air writes without allocation.
- [ ] Confirm RED, implement types and storage, confirm GREEN.
- [ ] Add deterministic iteration/snapshot APIs for renderer and persistence.
- [ ] Run `cargo test -p world` and full workspace tests; commit.

## Task 4: CPU chunk meshing

**Files:**
- Create: `rust/crates/renderer/Cargo.toml`
- Create: `rust/crates/renderer/src/lib.rs`
- Create: `rust/crates/renderer/src/mesh.rs`
- Test: mesh unit tests

**Interfaces:**
- `Vertex { position: [f32; 3], normal: [i8; 3], block: u16 }`.
- `ChunkMesh { vertices: Vec<Vertex>, indices: Vec<u32> }`.
- `mesh_section(section, neighbors, registry) -> ChunkMesh`.
- Opaque neighboring blocks hide shared faces; air and non-opaque neighbor behavior are tested explicitly.

- [ ] Write RED tests for a single cube (6 faces), two adjacent opaque blocks (10 exposed faces), empty section (0 vertices), and boundary neighbors.
- [ ] Implement a deterministic face-culling mesher using fixed face templates and contiguous output buffers.
- [ ] Add a byte/vertex-count regression test and a native release benchmark fixture.
- [ ] Run renderer and workspace tests; commit.

## Task 5: Versioned mod API and sandbox

**Files:**
- Create: `rust/crates/mod-api/Cargo.toml`
- Create: `rust/crates/mod-api/src/lib.rs`
- Create: `rust/crates/mod-runtime/Cargo.toml`
- Create: `rust/crates/mod-runtime/src/lib.rs`
- Create: `rust/mod-sdk/README.md`
- Create: `rust/mod-sdk/examples/minimal-mod/`

**Interfaces:**
- ABI version constant and manifest validation.
- Namespaced registrations and explicit capability declarations.
- Runtime loads independent `.wasm` modules with a bounded memory/fuel budget.
- No direct host filesystem or network imports by default.

- [ ] Write tests for ABI mismatch, invalid manifest, ungranted capability, fuel exhaustion, and one module failing without disabling others.
- [ ] Implement a small `wasmi` host boundary; avoid per-vertex callbacks and other high-frequency rendering hooks.
- [ ] Add minimal Rust mod example and SDK contract tests.
- [ ] Run mod tests and workspace suite; commit.

## Task 6: Browser lifecycle and GPU renderer

**Files:**
- Create: `rust/crates/client-web/Cargo.toml`
- Create: `rust/crates/client-web/src/lib.rs`
- Create: `rust/crates/client-web/src/browser.rs`
- Create: `rust/crates/renderer/src/gpu.rs`
- Create: `rust/crates/renderer/shaders/voxel.wgsl`
- Create: `rust/web/index.html`
- Create: `rust/scripts/build-web.sh`

**Interfaces:**
- Browser entry exports a single `start(canvas_id)` initialization function.
- Renderer requests WebGPU first, then WebGL2, and returns a structured error when neither exists.
- Render loop uses browser animation-frame callbacks and a fixed/update timestep for simulation.
- GPU buffers consume `ChunkMesh` data; game simulation remains independent of graphics APIs.

- [ ] Add browser initialization tests and a browser smoke-test page.
- [ ] Implement adapter selection, surface configuration, depth target, simple voxel shader, camera matrices and mesh draw.
- [ ] Compile to `wasm32-unknown-unknown`; verify WebGPU and WebGL2 code paths with separate browser checks.
- [ ] Enforce the 8 GiB / 20-minute clean browser build gate; commit.

## Task 7: Data, resource packs, and saves

**Files:**
- Create: `rust/crates/assets/`
- Create: `rust/crates/persistence/`
- Create: format compatibility tests and reference fixtures

- [ ] Add parsers for the required Minecraft asset/model/texture and NBT data formats.
- [ ] Port resource-location, palette, block-state and chunk serialization semantics.
- [ ] Add browser persistence (IndexedDB) with schema versioning and corruption recovery.
- [ ] Test round trips and compare selected outputs with fixtures from the Java reference.

## Task 8: Gameplay vertical slices

Port and test behavior in working slices, rather than copying classes mechanically:
1. Input, camera, movement, collision, gravity, and player state.
2. Block interaction, breaking/placing, tools, items, inventory, and hotbar.
3. World generation, biomes, lighting, fluids, and weather.
4. Entities, mobs, AI, combat, effects, block entities, and menus.
5. Resource reloads, sound, particles, HUD, accessibility, and options.

Each slice must have parity fixtures or documented behavior tests before the next slice is accepted.

## Task 9: Networking and singleplayer/multiplayer

- [ ] Port the Eaglercraft-compatible protocol layer into Rust and test packet encoders/decoders against captured reference fixtures.
- [ ] Add WebSocket transport, reconnect/error states, compression limits, and server-authoritative gameplay validation.
- [ ] Implement singleplayer host/server behavior in Rust rather than relying on the Java worker at final cutover.
- [ ] Test malformed packets, disconnects, stale state and resource exhaustion.

## Task 10: Cutover and removal of legacy browser client

- [ ] Compare critical gameplay, world data, protocol behavior, resource loading and persistence with the reference implementation.
- [ ] Remove `target_teavm` JavaScript output from the default release workflow only after Rust parity gates pass.
- [ ] Preserve source/reference Java code and optional developer targets until owner requests their deletion.
- [ ] Verify the final release contains Rust-produced WASM and only the minimal generated JS browser glue.
- [ ] Publish benchmark report with device, GPU backend, view distance, resolution, FPS and memory data.

---

## Execution rulings

- The owner explicitly requested continuous progress and authorized temporary Java use, so Java references/compatibility tooling may remain during migration; no Java engine dependency may remain in the final browser client.
- No implementation commit may be represented as full Minecraft 26.2 parity until Tasks 7–10 are actually complete and tested.
- The 8 GiB / 20-minute requirement will be verified through measured constrained builds. If it cannot be met with the current dependency graph, dependencies and build settings must be reduced before the gate is relaxed.
