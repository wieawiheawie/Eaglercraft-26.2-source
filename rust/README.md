# Eaglercraft 26.2 — Rust client

This is the new Rust-owned browser client. It is deliberately isolated from the existing Java/TeaVM workspace while the feature port is in progress.

## Build requirements

- Rust stable via rustup
- `wasm32-unknown-unknown` target (installed by `rust-toolchain.toml`)
- 8 GiB RAM is a hard ceiling; smaller builders are preferred when the measured build fits.
- Two Cargo jobs by default to limit peak compiler memory.

## Commands

From this directory:

```sh
cargo test --workspace
cargo build --release --target wasm32-unknown-unknown -p eagler-client-web
```

The CI browser build is limited to 20 minutes and records peak resident memory. The 7 GiB process-memory gate reserves headroom for the operating system. Passing the workspace tests alone does not mean the Minecraft port is complete.

## Porting rule

New game behavior belongs in Rust crates. Existing Java code is retained as a temporary reference/compatibility oracle, not as the final runtime. Generated browser bindings are allowed, but no JavaScript game engine or gameplay implementation is allowed.

## Current milestone

The workspace is scaffolding only. Game features, browser renderer, save compatibility, networking and the mod runtime are added in tested slices. Do not remove the Java reference or its build targets until parity gates pass.
