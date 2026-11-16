//! Byte grouping now lives in the pack crate, because it is part of the
//! on-disk format (record codec 2). Re-exported here for compatibility.
pub use o5s_pack::bytegroup::*;
