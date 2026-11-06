//! Browser entry point for the Rust-owned client.

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod wisp_browser;

#[cfg(target_arch = "wasm32")]
pub use wisp_browser::BrowserWispConnection;
