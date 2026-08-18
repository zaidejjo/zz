//! ZZ standard library (Phase 2).
//!
//! Two registries, kept in lockstep:
//! - [`stdlib_funcs`]: type signatures consumed by the checker.
//! - [`stdlib_natives`]: Rust implementations consumed by the interpreter.
//!
//! Modules:
//! - `std.io`  — `printz`, `println`, `read_line`
//! - `std.str` — `length`, `split`, `contains`
//! - `std.vec` — `push`, `pop`, `len`

pub mod funcs;
pub mod natives;

pub use funcs::stdlib_funcs;
pub use natives::stdlib_natives;

/// The set of known `std.*` module names (second path component).
pub const STDLIB_MODULES: &[&str] = &["io", "str", "vec"];
