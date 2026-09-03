//! ZZ native codegen backend (Phase 3).
//!
//! Consumes the DCE-pruned typed program from [`zz_hir`], lowers it to C,
//! embeds the C runtime, and invokes a C compiler to produce a standalone
//! binary. `cc`/`clang`/`gcc` are auto-detected; NO manual tooling install
//! is required on standard systems (they ship with the OS toolchain).

pub mod compile;
pub mod lower;

pub use compile::{compile_and_run, detect_cc, BuildError, BuildOptions};
pub use lower::{mangle, native_supported, LoweredC, Lowerer};

/// The embedded C runtime header.
pub const RUNTIME_H: &str = include_str!("runtime.h");
/// The embedded C runtime implementation.
pub const RUNTIME_C: &str = include_str!("runtime.c");

/// Version marker for generated binaries.
pub const C_BUILD_VERSION: &str = "0.1.0";

/// Build a native binary from a pruned typed program.
///
/// Returns the generated C source (for tests/inspection) and the path to
/// the compiled binary.
pub fn build_native(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    out_path: &std::path::Path,
) -> Result<LoweredC, BuildError> {
    // Only emit reachable natives for which we have a C impl; the rest are
    // pruned by DCE anyway. If a reachable native lacks a C impl, lower to
    // unit (documented limitation of the MVP runtime).
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
    );
    let lowered = lowerer.lower(tp);
    compile::build(&lowered.source, out_path, opts)?;
    Ok(lowered)
}

#[cfg(test)]
mod tests;
