//! Physical layer of pico2peta: append-only packs with a sorted index,
//! bundling of small objects, FastCDC chunking of large ones, crash
//! recovery, and multi-process safety. Sealed packs live on any
//! [`pico2peta_core::backend::Backend`].

pub mod format;
pub mod store;

pub use store::{PackConfig, PackStats, PackStore};
