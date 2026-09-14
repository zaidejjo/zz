//! ZZ native codegen backend (Phase 3).
//!
//! Consumes the DCE-pruned typed program from [`zz_hir`], lowers it to C,
//! embeds the C runtime, and invokes a C compiler to produce a standalone
//! binary. `cc`/`clang`/`gcc` are auto-detected; NO manual tooling install
//! is required on standard systems (they ship with the OS toolchain).

pub mod cache;
pub mod compile;
pub mod ffi;
pub mod lower;

pub use compile::{
    compile_and_run, compile_and_run_for_target, detect_clang, detect_clang_with,
    emit_c_plus_script, host_triple, is_macos_target, is_windows_target, validate, BuildError,
    BuildOptions, Clang, ClangProvider, PgoMode,
};
pub use ffi::{ffi_impl, FfiError, FFI_VERSION};
pub use lower::{mangle, native_supported, LoweredC, Lowerer};

/// The embedded C runtime header. The runtime is split across modular
/// sub-headers under `src/runtime/`; the umbrella `runtime.h` includes them
/// in dependency order, and they are concatenated here into one string.
pub const RUNTIME_H: &str = concat!(
    include_str!("runtime/runtime.h"),
    include_str!("runtime/platform.h"),
    include_str!("runtime/core.h"),
    include_str!("runtime/memory.h"),
    include_str!("runtime/strings.h"),
    include_str!("runtime/collections.h"),
    include_str!("runtime/json.h"),
);

/// The embedded C runtime implementation. The modular `.c` files are
/// concatenated in dependency order into a single translation unit.
pub const RUNTIME_C: &str = concat!(
    include_str!("runtime/memory.c"),
    include_str!("runtime/strings.c"),
    include_str!("runtime/collections.c"),
    include_str!("runtime/json.c"),
    include_str!("runtime/core.c"),
);

/// Version marker for generated binaries.
pub const C_BUILD_VERSION: &str = "0.1.0";

/// Lower a pruned typed program to C without compiling.
///
/// Used by dev builds (`zz build` default): the C is dumped to `bin/app.c`
/// for inspection while execution stays on the VM — no toolchain involved.
pub fn lower_only(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
) -> LoweredC {
    // Only emit reachable natives for which we have a C impl; the rest are
    // pruned by DCE anyway. If a reachable native lacks a C impl, lower to
    // unit (documented limitation of the MVP runtime).
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.lower()
}

/// Build a native binary from a pruned typed program.
///
/// `target` is the `--target=<triple>` cross triple, or `None` for a
/// native host build. Returns the generated C source (for
/// tests/inspection) and the path to the compiled binary.
pub fn build_native(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    out_path: &std::path::Path,
) -> Result<LoweredC, BuildError> {
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    let lowered = lowerer.lower();
    // Programs calling Rust-staticlib natives need the native runtime link
    // even when the caller did not opt in explicitly.
    let mut opts = opts;
    opts.native_rt = opts.native_rt || lowered.needs_native_rt;
    compile::build(&lowered.source, out_path, opts, target)?;
    Ok(lowered)
}

/// [`build_native`] with an explicit Clang provider (honors `--cc`).
pub fn build_native_with(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &Clang,
    out_path: &std::path::Path,
) -> Result<LoweredC, BuildError> {
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    let lowered = lowerer.lower();
    let mut opts = opts;
    opts.native_rt = opts.native_rt || lowered.needs_native_rt;
    compile::build_with(&lowered.source, out_path, opts, target, clang)?;
    Ok(lowered)
}

#[cfg(test)]
mod tests;
