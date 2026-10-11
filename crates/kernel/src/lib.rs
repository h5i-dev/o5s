//! Verified pure kernels for o5s patch admission, graph projection and byte rendering.
//!
//! Charon and Aeneas extract the admission, graph traversal, interval and
//! line-coordinate functions called by the application into Lean 4. Their
//! specifications are checked in `proofs/`. The native document adapter calls
//! those traversals; its complete operation semantics remain unproved.

pub mod coordinates;
pub mod graph;
pub mod indexed;
pub mod interval;
/// Native edit-graph adapter; only its indexed traversal has Lean proofs.
pub mod model;
pub mod patch;

pub mod order;

pub mod tags;
