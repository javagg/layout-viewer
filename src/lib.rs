pub mod core;
pub mod graphics;
pub mod rsutils;

#[cfg(target_arch = "wasm32")]
pub mod webui;

#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
