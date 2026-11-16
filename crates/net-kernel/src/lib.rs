//! pico2peta's front decisions and wire parsers, written in the Rust subset that Charon and
//! Aeneas extract to Lean 4 (h5i-app). The theorems in `verify/net/proofs` are about this
//! code as extracted. The server calls these functions; it does not reimplement them.

pub mod parse;
