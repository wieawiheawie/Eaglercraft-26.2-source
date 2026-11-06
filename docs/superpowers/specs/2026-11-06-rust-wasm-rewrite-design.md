# Eaglercraft 26.2 Rust/Wasm Rewrite — Design Proposal

**Date:** 2026-11-06  
**Status:** Proposed — awaiting owner approval  
**Base:** `main` of `wieawiheawie/Eaglercraft-26.2-source`

## 1. Intent

Replace the existing Java/TeaVM browser client with a new Rust-first Minecraft 26.2-compatible client compiled to WebAssembly. The target audience is school Chromebooks and other low-memory browser devices. The replacement must be modding-friendly, run in a normal browser tab, and have a predictable build that fits within an 8 GiB build machine and a 20-minute compile/package ceiling.

This is a real engine and gameplay port. It is not a Java-to-Rust transpilation, a TeaVM setting change, or a thin Rust wrapper around the existing Java game.

## 2. What the repository contains today

The fork contains a patched Java source tree with 7,142 Java files. The game is under `game/src/main/java`; the browser platform is under `platform-teavm`; the current Wasm-GC client, mesh worker and server targets are `target_teavm_wasm_gc*`. The distinct JavaScript target is `target_teavm`.

The existing Wasm-GC build is still Java compiled through TeaVM. Removing only `target_teavm` would not make the game Rust. The project also contains Minecraft renderer and chunk-compilation logic, including worker-routed mesh compilation and per-frame mesh budgets. Those behaviors are useful porting references, not reusable Rust implementation.

The existing build guide reports a 10 GiB minimum build budget plus system memory, recommends 16 GiB, and records a 41m46s local build. The current TeaVM linker scripts also reserve multi-gigabyte heaps. Those build settings cannot meet the new 8 GiB / 20-minute requirement; the Rust build must be designed and measured independently.

## 3. Goals

- Rust owns the game runtime: world state, simulation, entities, inventory, items, blocks, UI state, rendering, input, networking, persistence, and mod loading.
- The browser build is WebAssembly and can run without a native launcher or virtual desktop.
- Use WebGPU when available and WebGL2 as the compatibility fallback.
- Expose a stable, documented mod API with independently distributed mods.
- Support the relevant Minecraft 26.2 gameplay and data formats through deliberate porting and compatibility tests.
- Make an 8 GiB build machine a supported environment, with a clean release build and web packaging completing in no more than 20 minutes on the agreed reference machine.
- Keep the build reproducible, cacheable, and diagnosable, with memory and duration measurements in CI.
- Aim for smooth play on school Chromebook-class hardware; treat 120 FPS as a measured performance goal for suitable scenes/devices, not a guarantee for every integrated GPU.

## 4. Non-goals

- No automatic or wholesale Java-to-Rust source translation. The game behavior will be ported in tested slices.
- No Bevy-sized all-in-one engine dependency. A focused renderer and runtime are preferred to keep dependency count, compile memory, and binary size controlled.
- No native DLL/SO mods in the browser.
- No promise of complete feature parity in the first playable milestone.
- No distribution of game assets or content unless the repository owner has the rights to distribute them.

## 5. Approaches considered

### A. Full Rust replacement — selected

Build a new Rust engine and browser client, port gameplay and formats in slices, and eventually remove the legacy TeaVM client targets from the normal release path. This is the only approach that makes the game runtime genuinely Rust and gives us a coherent mod API.

### B. Hybrid Java/Wasm engine with Rust modules

Keep most of the Java game compiled through TeaVM and use Rust only for selected hot paths. This is less risky and can improve individual systems, but it does not satisfy the request for a fully Rust game.

### C. Automated Java-to-Rust conversion

Attempt to translate the Java source tree mechanically. Java's object model, libraries, threading, renderer contracts, and runtime assumptions do not map cleanly to Rust. A generated translation would be difficult to debug and would not deliver a trustworthy or mod-friendly foundation.

**Decision:** Approach A.

## 6. Proposed Rust architecture

Use a Cargo workspace with small, independently testable crates:

- `client-web`: browser entry point, lifecycle, input, canvas, browser storage and web-facing platform bindings.
- `game-core`: deterministic game rules, registries, entity/component state, ticks, inventory and gameplay events.
- `world`: chunks, block storage, terrain, lighting, world generation and save serialization.
- `renderer`: custom voxel renderer using `wgpu`, chunk visibility, mesh buffers, shaders, render scheduling and GPU resource management.
- `assets`: resource packs, texture/model/shader loading, atlas preparation and asset validation.
- `protocol`: versioned network packets and client connection state.
- `persistence`: worlds, player data, settings and IndexedDB/browser save integration.
- `mod-api`: stable versioned ABI, registries, events, capabilities and public types.
- `mod-runtime`: runtime loading and sandboxed execution of mod Wasm modules.
- `mod-sdk`: developer-facing Rust crate and examples for creating mods.
- `xtask`: Rust build/test/package commands so the project does not depend on a Node-based game build pipeline.

Keep the workspace dependency graph acyclic and avoid compiling browser-only APIs into native test targets. Core game logic must also compile natively so unit tests can run quickly without a browser.

### Renderer and browser integration

Use `wgpu` directly rather than adopting a large general-purpose engine. Prefer WebGPU, then fall back to WebGL2. Keep shader and rendering features inside the common capability set until device tests prove otherwise. The WebGL2 path is a compatibility backend and does not expose every feature available on WebGPU.

The WebAssembly target will be `wasm32-unknown-unknown`. Use `wasm-bindgen` / `web-sys` only at the narrow browser boundary for canvas, input, animation-frame scheduling, fetch, WebSocket and IndexedDB integration.

**JavaScript clarification:** the game runtime and application logic will be Rust. Browser WebAssembly builds may still include a small JavaScript loader/binding shim generated by `wasm-bindgen`; this is browser interop glue, not a JavaScript game client. No hand-written JavaScript gameplay engine is planned.

## 7. Mod system proposal

Default proposal: mods are separate `.wasm` files compiled using the official Rust mod SDK and loaded at runtime. The client hosts them through a small Wasm ABI and an embedded `wasmi` interpreter, rather than relying on native dynamic libraries or browser-specific JS plugin code.

The API will be versioned and capability-based. Initial registration/event surfaces should include blocks, items, recipes, entities, commands, resource assets, client input/UI events and gameplay hooks. Rendering extensions will use a constrained renderer-facing API rather than direct access to internal GPU handles.

Sandbox rules:
- no direct filesystem or arbitrary network access;
- explicit capabilities for game data and actions;
- bounded linear memory, module count and execution fuel;
- per-hook time budgets and fault isolation;
- mod ABI version checks and clear load errors;
- server-authoritative validation for multiplayer gameplay changes.

The interpreter is appropriate for untrusted extension code and supports fuel metering, but it is slower than native execution. Therefore, mod callbacks must not run per-vertex or in other high-frequency rendering loops. Mods should register data and receive coarse-grained events; hot-path processing stays in the Rust engine.

## 8. 8 GiB / 20-minute build design

The build requirement is an acceptance gate, not an assumption. The reference test will run on a Linux builder constrained to 8 GiB RAM. The compiler/tool process peak must stay below 7 GiB, reserving the remainder for the OS and browser/build services.

Initial build policy:
- pin Rust stable, target, Cargo lockfile and `wasm-bindgen-cli` version;
- set `CARGO_BUILD_JOBS=2` as the starting point, adjusting only from measured peak RSS and elapsed time;
- no link-time optimization in the normal profile;
- use `opt-level = 2`, no debug symbols in release, `panic = "abort"`, and a moderate codegen-unit count;
- separate a fast incremental development profile from the smaller optimized release profile;
- compile only the browser client and its required crates for the default web build;
- avoid compiling desktop, server, mesh-worker or diagnostic targets unless explicitly requested;
- use Cargo and CI caches, but also measure a clean source build with dependencies already fetched so network variability does not masquerade as compiler performance;
- make expensive extra minification an optional step, not a prerequisite for a valid build.

Required build acceptance test:
1. On the named 8 GiB reference builder, perform a clean release compile and web packaging from a clean source tree, with the pinned toolchain and dependency cache populated.
2. Record wall-clock time and peak memory.
3. Pass only if the complete compile/package finishes within 1,200 seconds and stays below 7 GiB peak process-tree memory.
4. Run a separate incremental build test and preserve build timings as CI artifacts.
5. If the first implementation misses the gate, reduce dependencies, split heavy crates, reduce concurrent jobs or simplify optimization settings before expanding the port.

There is no artificial one-minute delay: faster builds are better. The hard limit is 20 minutes. Toolchain installation and the first download of dependencies are reported separately from compile/package time.

This target is a design requirement, not yet a demonstrated result. It can only be claimed as met after the constrained build test passes.

## 9. Performance and compatibility validation

- Test WebGPU and WebGL2 independently; never infer WebGL2 success from a WebGPU run.
- Add deterministic tests for block state, collision, inventory, item behavior, world serialization and protocol codecs.
- Use reference fixtures from the existing Java implementation where licensing and test setup permit; compare data behavior, not Java implementation details.
- Profile chunk generation, mesh building, GPU upload, draw submission, memory use and frame pacing.
- Initial gameplay performance target: stable 60 FPS at a documented low-device profile and an aspirational 120 FPS in light scenes on capable hardware. Report actual device, view distance, resolution and scene load with every benchmark.
- Test saved-world round trips and a browser reload to verify persistence.
- Test multiple mod modules, invalid ABI versions, fuel exhaustion and one mod faulting without taking down the client.

## 10. Migration and release stages

1. **Rust/Wasm foundation:** create the workspace, minimal browser shell, GPU adapter selection, WebGPU/WebGL2 smoke tests, and the 8 GiB / 20-minute build gate. No legacy code is deleted.
2. **First playable slice:** render a small block world; add input, camera, chunk storage, chunk meshing, basic movement and collision.
3. **Core survival systems:** port world generation, block interactions, items, inventory, entities, UI, lighting, time and weather in vertical slices with tests.
4. **Persistence and multiplayer:** save/load browser worlds, protocol compatibility, connection handling and singleplayer/server strategy.
5. **Modding beta:** freeze the first mod ABI, publish SDK examples, load sandboxed `.wasm` mods, and implement registration/events/capability enforcement.
6. **Parity and cutover:** compare critical gameplay against the Java reference, then remove the legacy JS/TeaVM client from the normal release pipeline. Preserve Java sources and old build targets in the repository as read-only migration/reference material until the Rust version passes the parity gates.

Each stage must build and run before the next stage grows. The first milestone is a verified Rust browser shell, not an assertion that the whole Minecraft game has already been ported.

## 11. Approval boundary

This document proposes the architecture and defaults above. No Rust product code, Cargo workspace, dependency installation, or legacy target deletion should begin until the owner approves this design.

Calls left to the builder unless the owner corrects them:
- use `wgpu` with WebGPU and WebGL2 fallback;
- use a custom focused renderer instead of a full game engine framework;
- use sandboxed runtime `.wasm` mods through `wasmi` and a versioned Rust SDK;
- use a raw multi-file web build as the primary artifact, with optional single-HTML packaging;
- keep the old Java source and TeaVM targets as reference until parity is proven.
