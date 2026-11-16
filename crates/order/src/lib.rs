//! Similarity ordering and framing: one building block for every domain
//! (docs/ORDERING.md, evaluated by docs/PLAN.md §6.6).
//!
//! 1. [`features`](mod@features): content-defined-sampling super-features (Odess-style).
//! 2. [`order`](mod@order): a similarity order. LSH neighbours, greedy chaining, 2-opt.
//!    Front hints only break ties, and it works without any.
//! 3. [`frame`]: where to cut frames (where content turns dissimilar, or at a
//!    cap), and which earlier object a forced cut uses as a zstd reference
//!    prefix (bounded depth).
//!
//! Pure functions over lengths, features and optional hints. No I/O, no
//! compression, and nothing about any domain.
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod features;
pub mod frame;
pub mod order;

pub use features::{Features, features};
pub use frame::{Frame, FrameConfig, frames};
pub use order::{Hint, Item, order};
