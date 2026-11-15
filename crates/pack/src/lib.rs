//! Physical layer of pico2peta: append-only packs with a sorted index,
//! bundling of small objects, FastCDC chunking of large ones, crash
//! recovery, and multi-process safety. Sealed packs live on any
//! [`pico2peta_core::backend::Backend`].
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod format;
pub mod store;

pub use format::IdxEntry;
pub use store::{GC_PREFIX, PackConfig, PackStats, PackStore, read_name_list};
