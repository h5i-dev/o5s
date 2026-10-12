//! Verified pure kernels for o5s patch admission, graph projection and byte rendering.
//!
//! Charon and Aeneas extract the admission, graph traversal, interval and
//! line-coordinate functions called by the application into Lean 4. Their
//! specifications are checked in `proofs/`. The native document adapter calls
//! those traversals; its complete operation semantics remain unproved.

/// Causal searches over dense patch rows.
pub mod causality;
pub mod coordinates;
/// Dense graph-document kernel: patch validation, commit and materialization.
pub mod document;
pub mod graph;
pub mod indexed;
pub mod interval;
/// Line-range validation, merging and rendering, extracted and proved.
pub mod lines;
/// Native edit-graph adapter; traversal, frontier and tag scanners have Lean proofs.
pub mod model;
pub mod patch;

pub mod order;

pub mod tags;
