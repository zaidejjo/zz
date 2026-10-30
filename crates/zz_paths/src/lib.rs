//! Host-independent path utilities shared by the CLI, the package manager
//! and the AOT backends.
//!
//! This crate has no dependencies so that low-level consumers (notably the
//! Clang driver in `zz_codegen_cc`) can resolve cache directories without
//! pulling in the package manager. `zz_pm` re-exports both modules, so
//! existing `zz_pm::paths::*` / `zz_pm::pathsafe::*` imports keep working.
//!
//! - [`paths`]: `ZZ_HOME` resolution, cache/objects/bin layout, thread-scoped
//!   environment overrides for parallel tests.
//! - [`pathsafe`]: portable containment checks and verbatim-prefix stripping.
pub mod paths;
pub mod pathsafe;
