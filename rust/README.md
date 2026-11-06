# Eaglercraft 26.2 — Rust client

This is the new Rust-owned browser client. It is deliberately isolated from the existing Java/TeaVM workspace while the feature port is in progress.

## Build requirements

- Rust stable via rustup
- `wasm32-unknown-unknown` target (installed by `rust-toolchain.toml`)
- The browser build uses at most two Cargo jobs by default.
- 8 GiB RAM is an absolute ceiling; smaller builders are preferred when the measured build fits.
- The measured CI gate rejects builds above 7 GiB maximum resident set or 20 minutes.

## Commands

From this directory:

```sh
cargo test --workspace --locked
cargo build --release --target wasm32-unknown-unknown -p eagler-client-web
./scripts/build-web.sh
```

`build-web.sh` also requires the `wasm-bindgen` CLI version matching the locked `wasm-bindgen` crate. The output is a static multi-file browser site under `dist/`, including the checked-in Minecraft resource assets and data packs copied from `game/src/main/resources/`.

The CI browser build is limited to 20 minutes and records peak resident memory. Passing the workspace tests alone does not mean the Minecraft port is complete.

## Porting rule

New game behavior belongs in Rust crates. Existing Java code is retained as a temporary reference/compatibility oracle, not as the final runtime. Generated browser bindings are allowed, but no JavaScript game engine or gameplay implementation is allowed.

## Current milestone

The workspace contains a tested block registry, sparse world/chunk storage, CPU face-culling mesher, versioned mod manifests, a fuel/memory-limited Wasm mod runtime, a Wisp v1 TCP transport, and a Minecraft 26.2 status ping. The browser build decodes a starter set of the repository's real Minecraft PNG textures in Rust and uploads them to a GPU atlas. Full gameplay and multiplayer login parity are still under port.
