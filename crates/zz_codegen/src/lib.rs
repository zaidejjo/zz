//! ZZ native codegen facade.
//!
//! Consumes the DCE-pruned typed program from [`zz_hir`], lowers it to C,
//! embeds the C runtime, and invokes a C compiler to produce a standalone
//! binary. `cc`/`clang`/`gcc` are auto-detected; NO manual tooling install
//! is required on standard systems (they ship with the OS toolchain).
//!
//! The implementation is split into independently compileable crates and
//! re-exported here so downstream crates keep a single import surface:
//!
//! - [`zz_lower`]: HIR → C lowering
//! - [`zz_chunk`]: unified IR → C lowering (chunk backend)
//! - [`zz_codegen_cc`]: clang/gcc driver, flag matrix, runtime-archive cache
//! - [`zz_codegen_ffi`]: native + staticlib symbol tables, link metadata
//! - [`zz_codegen_runtime`]: embedded C runtime (single TU) + rebuild guard
//! - `zz_codegen_types`: build options, errors, compiler handles
//!
//! This crate keeps the orchestration (lower-only dumps, native/unit
//! builds) and the end-to-end pipeline tests.

pub use zz_chunk as chunk;
pub use zz_codegen_cc as compile;
pub use zz_codegen_cc::cache;
pub use zz_codegen_ffi as ffi;

pub use chunk::{build_module as build_chunk_module, coverage as chunk_coverage, ChunkError};
pub use compile::{
    build_options_fingerprint, build_options_fingerprint_with, compile_and_run,
    compile_and_run_for_target, detect_clang, detect_clang_with, emit_c_plus_script, host_triple,
    is_macos_target, is_windows_target, validate, BuildError, BuildOptions, Clang, ClangProvider,
    EmbedAsset, PgoMode,
};
pub use ffi::{ffi_decl, ffi_impl, native_impl, native_supported, FfiError, FFI_VERSION};
pub use zz_codegen_runtime::{C_BUILD_VERSION, RUNTIME_C, RUNTIME_H, RUNTIME_SRC_HASH};
pub use zz_lower as lower;
pub use zz_lower::{mangle, units, LoweredC, Lowerer};

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
    // `compile::build` is exactly "detect a provider, then build"; mapping
    // the missing-provider case here keeps the single implementation below.
    let clang = compile::detect_clang().ok_or(BuildError::NoClang)?;
    build_native_with(tp, reach, entry_main, opts, target, &clang, out_path)
}

/// Lower a pruned program in precompiled-runtime mode.
///
/// All native build entry points share this: the lowering mode and the
/// `Lowerer` setup must not drift between the single-TU and per-unit paths.
fn precompiled_lowerer(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
) -> Lowerer {
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    lowerer
}

/// Per-module native build: lower to namespaced translation units and
/// compile them (serially for now) against the precompiled runtime archive.
/// `--embed` tables ride in the entry unit (emitted once, never in the
/// shared header). Returns the entry-unit index for callers that need it.
pub fn build_native_units_with(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &compile::Clang,
    out_path: &std::path::Path,
) -> Result<units::LoweredUnits, compile::BuildError> {
    // Entry namespace derives from the dotted main key (`main.main` ->
    // `main`); bare test mains land in the prelude bucket with bare fns.
    let entry_ns = units::ns_of(entry_main).to_string();
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    let mut lowered = precompiled_lowerer(tp, reach, entry_main).lower_units(&entry_ns);
    if verbose {
        eprintln!("zz: timing lower_units={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    lowered.apply_link_needs(&mut opts);
    if !opts.embed_assets.is_empty() {
        let tables = compile::embed_c(&opts.embed_assets);
        match lowered.units.iter_mut().find(|u| u.ns == lowered.entry_ns) {
            Some(u) => {
                u.code.push_str(&tables);
            }
            None => {
                lowered.units.push(units::CodeUnit {
                    ns: lowered.entry_ns.clone(),
                    code: tables,
                });
            }
        }
    }
    let pairs: Vec<(String, String)> = lowered
        .units
        .iter()
        .map(|u| (u.ns.clone(), u.code.clone()))
        .collect();
    let t_cc = std::time::Instant::now();
    compile::compile_units(&lowered.header, &pairs, out_path, &opts, target, clang)?;
    if verbose {
        eprintln!("zz: timing units_cc_link={}ms", t_cc.elapsed().as_millis());
    }
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
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    let lowered = precompiled_lowerer(tp, reach, entry_main).lower();
    if verbose {
        eprintln!("zz: timing lower={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    lowered.apply_link_needs(&mut opts);
    let t_cc = std::time::Instant::now();
    compile::build_with(&lowered.source, out_path, opts, target, clang)?;
    if verbose {
        eprintln!("zz: timing clang_link={}ms", t_cc.elapsed().as_millis());
    }
    Ok(lowered)
}

/// Native e2e suite: lowers, compiles with the system C toolchain, runs the
/// binary and diffs stdout against the VM.
///
/// Skipped on Windows: the C runtime is POSIX-thread based (`core.h`
/// includes `<pthread.h>`, the channel/rep-pool code uses mutexes and
/// condition variables) and the socket layer is written against POSIX
/// `fcntl`/`poll`, so the AOT backend cannot build there yet. Windows CI
/// runs the VM suites and the CLI/PM tests; native legs are gated by
/// `ZZ_SKIP_NATIVE` in the integration tests for the same reason.
#[cfg(all(test, not(windows)))]
mod tests;
