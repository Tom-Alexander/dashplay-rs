//! Platform-specific runtime primitives.
//!
//! This module keeps target-specific differences in one place so playback code can stay
//! platform-agnostic.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
