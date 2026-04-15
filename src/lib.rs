//! Library surface for out-of-binary consumers (benchmarks, external tests).
//!
//! The binary is defined in `main.rs` and pulls the same modules in directly.
//! This file re-exports the parser so Criterion benches in `benches/` can
//! drive it without copying source.

pub mod vte;
