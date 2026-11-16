//! Physical layer of o5s: append-only packs with a sorted index,
//! bundling of small objects, FastCDC chunking of large ones, crash
//! recovery, and multi-process safety. Sealed packs live on any
//! [`o5s_core::backend::Backend`].
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod bytegroup;
pub mod format;
pub mod phash;
pub mod store;

pub use format::IdxEntry;
pub use o5s_chunk::XET_MAX;
pub use store::{ChunkerKind, GC_PREFIX, PackConfig, PackStats, PackStore, PutOpts, read_name_list};
